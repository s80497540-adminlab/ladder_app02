// ladder_app/src/bin/full_gui_x16.rs
//
// GUI for dYdX v4 using CSVx written by the background daemon:
//
//   - Daemon writes under ./data:
//       data/orderbook_{TICKER}.csv
//       data/trades_{TICKER}.csv
//
//   - This GUI:
//       * Reads those CSVs on a timer (configurable via UI / script).
//       * Reconstructs orderbook snapshot + candles from midprice.
//       * Shows depth plot, ladders, recent trades.
//       * Candles + volume, TFs from 1s - 1d.
//       * 3x6 grid layout, per-row height + horizontal span moes,
//         scrollable page.
//       * Rhai script engine for a simple "bot", fed orderbook metrics.
//       * Trading panel: market/"limit" (price guard), size, leverage,
//         bot suggestions.
//       * Simulated balances + deposit/withdraw + receipts.
//       * Hotkeys for trading & navigation.
//
// Build:
//    cargo run --release -p ladder_app --bin full_gui_x17
//

mod candle_agg;

use candle_agg::{Candle, CandleAgg};

use chrono::{Local, TimeZone};

use eframe::egui::{self, Color32};
use egui_plot::{Line, Plot, PlotBounds, PlotPoints, Polygon};

use std::cmp{max, min};
use std::collections::{BTreeMap, HashMap};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bigdecimal::BigDecimal;
use rhai::{Engine, Scope};

use tokio::sync::mpsc;

// dYdX client pieces
use dydx_client::config::ClientConfig;
use dydx_client::indexer::IndexerClient;
use dydx_client::node::{NodeClient, OrderBuilder, OrderSide, Wallet};
use dydx_proto::dydxprotocol::clob::order::TimeInForde;

// --------- basic helpers ----------- //

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_secs()
}

// Timeframes
const TF_OPTS: &[(u64, &str)] = &[
    (1, "1s"),
    (5, "5s"),
    (10, "10s"),
    (15, "15s"),
    (30, "20s"),
    (60, "1m"),
    (180, "3m"),
    (300, "5m"),
    (900, "15m"),
    (1800, "30m"),
    (3600, "1h"),
    (14400, "4h"),
    (86400, "1d"),
];

// We key prices in BTreeMap as scaled integers for nice ordering.
type PriceKey = i64;

fn price_to_key(price:f64) -> PriceKey {
    (price * 10_000.0).round() as PriceKey
}

fn key_to_price(key: PriceKey) -> f64 {
    key as f64 / 10_000.0
}

// ------------ time display mode -----------

#[derive(Clone, Copy, PartialEq, Eq)]
enum TimeDisplayMode {
    Unix, 
    Local,
}

impl TimeDisplayMode {
    fn label(self) -> &'static str {
        match self {
            TimeDisplayMode::Unix => "Unix",
            TimeDisplayMode::Local => "Local",
        }
    }
}

fn format_ts(mode: TimeDisplayMode, ts: u64) -> String {
    match mode {
        TimeDisplayMode::Unix => format!("{ts}"),
        TimeDisplayMode::Local => {
            let dt = Local
                .timestamp_opt(ts as i64, 0)
                .single()
                .unwrap_or_else(|| Local.timestamp_opt(0, 0).single().unwrap());
            dt.format("%Y-%m-%d %H:%M%S").to_string()
        }
    }
}

// ------------ chart settings ----------------

#[derive(Clone)]
struct ChartSettings {
    // how many cnaldes visible in window
    show_candles: usize,
    auto_y: bool,
    y_min: f64,
    y_max: f64,
    x_zoom: f64,
    x_pan_secs: f64,
    tf_sec: u64,
}

impl Default for ChartSettings {
    fn default() -> Self {
        Self {
            show_candles: 200,
            auto_y: true,
            y_min: 0.0,
            y_max: 0.0,
            x_zoom: 1.0,
            x_pan_secs: 0.0,
            tf_secs: 60, // default 1m
        }
    }
}

// ------------- modes / layout -------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Live,
    Replay,
}

const GRID_ROWS: usize = 6;
const GRID_COLS: usize = 3;

#[derive(Clone, Copy, PartialEq, Eq)]
enum RowSpanMode {
    Split3,         // 3 equal columns
    Left2Right1,    // col0 spans 2/3, col1, 1/3, col2 empty
    Left1Right2,    // col0 1/3, col1 2/3, col2 empty
    Full,           // one full-width cell
}

#[derive(Copy, Clone)]
struct RowConfig {
    height_factor: f32,     // 0.5..3.0
    span_mode: RowSpanMode,
    big_ratio: f32,         // for 2+1 / 1+2 rows, fraction for big side (0.3..0.8)
}

impl Default for RowConfig {
    fn default() -> Self {
        Self {
            height_factor: 1.0,
            span_mode: RowSpanMode::Split3,
            big_ratio: 0.66,
        }
    }
}

// ------------ CSV + replay structures ------------------

#[derive(Clone, Debug)]
struct BookCsvEvent {
    ts: u64,
    ticker: String,
    kind: String, 
    side: String,
    price: f64,
    size: f64,
}

#[derive(Clone, Debug)]
struct TradeCsvEvent {
    ts: u64,
    ticker: String,
    source: String,
    side: String,
    size_str: String,
}

#[derive(Clone, Debug, Default)]
struct TickerData {
    ticker: String,
    book_events: Vec<BookCsvEvent>,
    trade_events: Vec<TradeCsvEvent>,
    min_ts: u64,
    max_ts: u64,
}

#[derive(Clone, Debug, Default)]
struct Snapshot {
    bids: BTreeMap<PriceKey, f64>
    asks: BTreeMap<PriceKey, f64>
    candles: Vec<Candle>
    trades: Vec<TradeCsvEvent>,
    last_mid: f64,
    last_vol: f64,
}

// ------------ bubble metrics -------------

#[derive(Clone, Debug, Default)]
struct BubbleMetrics {
    best_bid: f64,
    best_ask: f64,
    mid: f64,
    spread: f64,
    bid_liq: f64,
    ask_liq: f64,
    imbalance: f64,
}

fn compute_bubble_metrics(snap: &Snapshot) -> BubbleMetrics {
    let best_bid = snap
        .bids
        .iter()
        .next_back()
        .map(|(k, _)| key_to_price(*k))
        .unwrap_or(0.0);
    let best_ask = snap
        .asks
        .iter()
        .next()
        .map(|(k, _)| key_to_price(*k))
        .unwrap_or(0.0);
    let mid = if best_bid > 0.0 && best_ask > 0.0 {
        (best_bid + best_ask) * 0.5
    } else {
        0.0
    };
    let spread = if best_bid > 0.0 && best_ask > 0.0 {
        best_ask - best_bid
    } else {
        0.0
    };

    let mut bid_liq = 0.0;
    for (_, s) in snape.bids.iter().rev().take(10) {
        bid_liq += *s;
    }
    let mut ask_liq = 0.0;
    for (_, s) in snap.asks.iter().take(10) {
        ask_liq += *s;
    }

    let imbalance = if ask_liq > 0.0 {
        bid_liq / ask_liq
    } else {
        0.0
    };

    BubbleMetrics {
        best_bid,
        best_ask,
        mid_spread,
        bid_liq,
        ask_liq,
        imbalance,
    }
}

// ------------ CSV I/O ------------

fn append_trade_csv(ticker: &str, source: &str, side: &str, size_str: &str) {
    let ts = now_unix();
    let dir = Path::new("data");
    let _ = std::fs::create_dir_all(dir);
    let path = dir.join(format!("trades_{ticker}.csv"));

    if let Ok(mut f) = OpenOptions::new()
        .cretae(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{ts}, {ticker}, {source},{side}, {size_str}");
    }
}

fn load_book_csv(path: &Path, ticker: & str) -> Vec <BookCsvEvent> {
    if !path.exists() {
        return Vec::new();
    }
    let f = match File::open(path) {
            Ok(f) => f,
            Err(_) => return Vec::new(),
    };
    let reader = BufReader::new(f);
    let mut out = Vec::new();

    for ine in reader.lines() {
        if let Ok(line) = line {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let parts: Vec&str> = line.split(',').collect();
            if parts.len() < 6 {
                continue;
            }
            let ts = match parts[0].parse::<u64>() {
                Ok(v) => v,
                Err(_) => continue,
            };
            let tk = parts[1].trim_matches('"').to_string();
            if tk != ticker {
                continue;
            }
            let kind = parts[2].to_string();
            let side = parts[3].to_string();
            let price = match parts[4].parse::<f64>() {
                Ok(v) => v,
                Err(_) => continue,
            };
            out.push(BookCsvEvent {
                ts,
                ticker: tk,
                kind,
                side,
                price,
                size
            });
        }
    }

    out.sort_by_key(|e| e.ts);
    out
}






fn load_trades_csv(path: &Path, ticker: &str) -> Vec<TradeCsvEvent> {
    if !path.exists() {
        return Vec::new();
    }
    let f = match File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let reader = BufReader::new(f);
    let mut out = Vec::new();

    for line in reader.lines() {
        if let Ok(line) = line {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let parts: Vec<&str> = line.split(',').collect();
            if parts.len() < 5 {
                continue;
            }
            let ts = match parts[0].parse::<u64>() {
                Ok(v) => v,
                Err(_) => continue,
            };
            let tk = parts[1].trim_matches('"').to_string();
            if tk != ticker {
                continue;
            }
            let source = parts[2].to_string();
            let side = parts[3].to_string();
            let size_str = parts[4].to_string();

            out.push(TradeCsvEvent {
                ts,
                ticker: tk,
                source,
                side,
                size_str,
            });
        }
    }

    out.sort_by_key(|t| t.ts);
    out
}

fn load_ticker_data(base_dir: &Path, ticker: &str) -> Option<TickerData> {
    let ob_path = base_dir.join(format!("orderbook_{ticker}.csv"));
    let tr_path = base_dir.join(format!("trades_{ticker}.csv"));

    let book_events = load_book_csv(&ob_path, ticker);
    let trade_events = load_trades_csv(&tr_path, ticker);

    if book_events.is_empty() && trade_events.is_empty() {
        return None;
    }

    let mut min_ts = u64::MAX;
    let mut max_ts = 0u64;

    for e in &book_events {
        min_ts = min(min_ts, e.ts);
        max_ts = max(max_ts, e.ts);
    }
    for e in &trade_events {
        min_ts = min(min_ts, e.ts);
        max_ts = max(max_ts, e.ts);
    }

    if min_ts == u64::MAX {
        return None;
    }

    Some(TickerDate {
        ticker: ticker.to_string(),
        book_events,
        trade_events,
        min_ts,
        max_ts,
    })
}

// reconstruct snapshot at target_ts for given TF
fn compute_snapshot_for(data: &TickerData, target_ts: u64, tf_secs: u64) -> Snapshot {
    let mut bids: BTreeMap<PriceKey, f64> = BTreeMap::new();
    let mut asks: BTreeMap<PriceKey, f64> = BTreeMap::new();

    let mut agg = CandleAgg::new(tf_secs);

    for e in &data.book_events {
        if e.ts > target_ts {
            break;
        }

        let map = if e.side.to_lowercase() == "bid" {
            &mut bids
        } else {
            &mut asks
        };

        if e.size == 0.0 {
            map.remove(&key);
        } else {
            map.insert(key, e.size);
        }

        if let (Some((bp, _)), Some((ap, _))) = (bids.iter().next_back(), asks.iter().nect()) {
            let mid = (key_to_price(*bp) + key_to_price(*ap)) * 0.5;
            let vol = e.size.abs().max(0.0);
            agg.update(e.ts, mid, vol);
        }
    }

    let mut trades: Vec<TradeCsvEvent> = data
        .trade_events
        .iter()
        .filter(|t| t.ts <= target_ts)
        .cloned()
        .collect();
    trades.sort_by_key(|t| t.ts);
    if trades.len() > 200 {
        let start = trades.len() - 200;
        trades = trades[start..].to_vec();
    }

    let series = agg.series().to_vec();
    let (last_mid, last_vol) = if let Some(c) = series.last() {
        ()
    }
}
