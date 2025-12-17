// ladder_app/src/bin/full_gui7.rs
//
// Combined Live + Replay GUI for dYdX v4 testnet (ETH-USD, BTC-USD, SOL-USD)
//
// Live mode:
//   - Connects to dYdX indexer testnet
//   - Streams orderbook deltas for selected ticker
//   - Builds live orderbook depth + ladders
//   - Builds mid-price candles (30s / 1m / 3m / 5m) + volume
//   - Mouse drag/zoom on both axes
//   - Shift + mouse wheel over candles/volume => Y-only zoom
//   - Real testnet market BUY/SELL buttons with CSV logging
//   - NEW: preloads candles from existing CSV history for current ticker
//
// Replay mode:
//   - Reads CSVs from ./data:
//       data/orderbook_{TICKER}.csv
//       data/trades_{TICKER}.csv
//   - Reconstructs book + candles + volume + recent trades
//   - Same candle engine as live mode
//
// Shared:
//   - Ticker dropdown: ETH-USD / BTC-USD / SOL-USD
//   - Time display: Unix vs Local
//   - Y-axis: auto vs manual (plus vertical zoom via Shift+scroll)
//   - Current candle kept roughly centered horizontally
//   - Layout & appearance controls (ratios, colors, body width)
//
// Run:
//   # for GUI only (no real trades needed):
//   cargo run -p ladder_app --bin full_gui7
//
//   # to enable real trades from the buttons:
//   export DYDX_TESTNET_MNEMONIC='...'
//   cargo run -p ladder_app --bin full_gui7
//

mod candle_agg;

use candle_agg::{Candle, CandleAgg};

use eframe::egui;
use egui::Color32;
use egui_plot::{Line, Plot, PlotBounds, PlotPoints, VLine};

use chrono::{Local, TimeZone};

use std::cmp::{max, min};
use std::collections::{BTreeMap, HashMap};
use std::env;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::{mpsc, watch};

// dYdX client
use bigdecimal::BigDecimal;
use std::str::FromStr;

use dydx_client::config::ClientConfig;
use dydx_client::indexer::{
    Feed as DxFeed, Feeds, IndexerClient, OrderbookResponsePriceLevel, OrdersMessage, Ticker,
};
use dydx_client::node::{NodeClient, OrderBuilder, OrderSide, Wallet};
use dydx_proto::dydxprotocol::clob::order::TimeInForce;

// ------------- basic helpers -------------

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_secs()
}

// integer keys so BTreeMap ordering is nice
type PriceKey = i64;

fn price_to_key(price: f64) -> PriceKey {
    (price * 10_000.0).round() as PriceKey
}

fn key_to_price(key: PriceKey) -> f64 {
    key as f64 / 10_000.0
}

// ------------- time formatting -------------

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
            dt.format("%Y-%m-%d %H:%M:%S").to_string()
        }
    }
}

// ------------- chart + layout settings -------------

#[derive(Clone)]
struct ChartSettings {
    show_candles: usize,
    auto_y: bool,
    y_min: f64,
    y_max: f64,
    x_zoom: f64,
    x_pan_secs: f64,
    selected_tf: u64,
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
            selected_tf: 60,
        }
    }
}

#[derive(Clone)]
struct LayoutSettings {
    ladders_height_ratio: f32,     // fraction of central height for ladders+trading
    depth_width_ratio: f32,        // fraction of width for depth plot
    volume_height_ratio: f32,      // fraction of candles+volume height for volume
    candle_body_width_factor: f32, // 0.3..1.0 of TF bucket width
}

impl Default for LayoutSettings {
    fn default() -> Self {
        Self {
            ladders_height_ratio: 0.35,
            depth_width_ratio: 0.45,
            volume_height_ratio: 0.3,
            candle_body_width_factor: 0.7,
        }
    }
}

#[derive(Clone)]
struct AppearanceSettings {
    bull_color: Color32,
    bear_color: Color32,
    volume_color: Color32,
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            bull_color: Color32::from_rgb(0, 200, 0),
            bear_color: Color32::from_rgb(220, 50, 50),
            volume_color: Color32::from_rgb(120, 170, 240),
        }
    }
}

// ------------- tabs + modes -------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReplayTab {
    Orderbook,
    Candles,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Live,
    Replay,
}

// ------------- live book -------------

#[derive(Clone, Debug, Default)]
struct LiveBook {
    bids: BTreeMap<PriceKey, f64>,
    asks: BTreeMap<PriceKey, f64>,
}

impl LiveBook {
    fn apply_levels(
        map: &mut BTreeMap<PriceKey, f64>,
        levels: Vec<OrderbookResponsePriceLevel>,
        side: &str,
        ticker: &str,
    ) {
        for lvl in levels {
            let price_bd = lvl.price.0;
            let size_bd = lvl.size.0;
            let p = price_bd.to_string().parse::<f64>().unwrap_or(0.0);
            let s = size_bd.to_string().parse::<f64>().unwrap_or(0.0);
            let key = price_to_key(p);

            if s == 0.0 {
                map.remove(&key);
            } else {
                map.insert(key, s);
            }

            append_book_csv(ticker, "delta", side, p, s);
        }
    }

    fn apply_initial(
        &mut self,
        bids: Vec<OrderbookResponsePriceLevel>,
        asks: Vec<OrderbookResponsePriceLevel>,
        ticker: &str,
    ) {
        self.bids.clear();
        self.asks.clear();

        for lvl in bids {
            let price_bd = lvl.price.0;
            let size_bd = lvl.size.0;
            let p = price_bd.to_string().parse::<f64>().unwrap_or(0.0);
            let s = size_bd.to_string().parse::<f64>().unwrap_or(0.0);
            let key = price_to_key(p);
            if s != 0.0 {
                self.bids.insert(key, s);
            }
            append_book_csv(ticker, "book_init", "bid", p, s);
        }

        for lvl in asks {
            let price_bd = lvl.price.0;
            let size_bd = lvl.size.0;
            let p = price_bd.to_string().parse::<f64>().unwrap_or(0.0);
            let s = size_bd.to_string().parse::<f64>().unwrap_or(0.0);
            let key = price_to_key(p);
            if s != 0.0 {
                self.asks.insert(key, s);
            }
            append_book_csv(ticker, "book_init", "ask", p, s);
        }
    }

    fn apply_update(
        &mut self,
        bids: Option<Vec<OrderbookResponsePriceLevel>>,
        asks: Option<Vec<OrderbookResponsePriceLevel>>,
        ticker: &str,
    ) {
        if let Some(b) = bids {
            Self::apply_levels(&mut self.bids, b, "bid", ticker);
        }
        if let Some(a) = asks {
            Self::apply_levels(&mut self.asks, a, "ask", ticker);
        }
    }

    fn mid(&self) -> Option<f64> {
        let bp = self.bids.iter().next_back();
        let ap = self.asks.iter().next();
        match (bp, ap) {
            (Some((b, _)), Some((a, _))) => {
                let pb = key_to_price(*b);
                let pa = key_to_price(*a);
                Some((pb + pa) * 0.5)
            }
            _ => None,
        }
    }
}

// ------------- CSV + replay structures -------------

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

#[derive(Clone, Debug)]
struct TickerData {
    ticker: String,
    book_events: Vec<BookCsvEvent>,
    trade_events: Vec<TradeCsvEvent>,
    min_ts: u64,
    max_ts: u64,
}

#[derive(Clone, Debug, Default)]
struct Snapshot {
    bids: BTreeMap<PriceKey, f64>,
    asks: BTreeMap<PriceKey, f64>,
    tf_30s: Vec<Candle>,
    tf_1m: Vec<Candle>,
    tf_3m: Vec<Candle>,
    tf_5m: Vec<Candle>,
    last_mid: f64,
    last_vol: f64,
    trades: Vec<TradeCsvEvent>,
}

// --- CSV IO ---

fn append_book_csv(ticker: &str, kind: &str, side: &str, price: f64, size: f64) {
    let ts = now_unix();
    let dir = Path::new("data");
    let _ = std::fs::create_dir_all(dir);
    let path = dir.join(format!("orderbook_{ticker}.csv"));

    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{ts},{ticker},{kind},{side},{price},{size}");
    }
}

fn append_trade_csv(ticker: &str, source: &str, side: &str, size_str: &str) {
    let ts = now_unix();
    let dir = Path::new("data");
    let _ = std::fs::create_dir_all(dir);
    let path = dir.join(format!("trades_{ticker}.csv"));

    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{ts},{ticker},{source},{side},{size_str}");
    }
}

fn load_book_csv(path: &Path, ticker: &str) -> Vec<BookCsvEvent> {
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
            if parts.len() < 6 {
                continue;
            }
            let ts = match parts[0].parse::<u64>() {
                Ok(v) => v,
                Err(_) => continue,
            };
            let tk = parts[1].trim_matches('"').to_string();
            let kind = parts[2].to_string();
            let side = parts[3].to_string();
            let price = match parts[4].parse::<f64>() {
                Ok(v) => v,
                Err(_) => continue,
            };
            let size = match parts[5].parse::<f64>() {
                Ok(v) => v,
                Err(_) => continue,
            };

            if tk != ticker {
                continue;
            }

            out.push(BookCsvEvent {
                ts,
                ticker: tk,
                kind,
                side,
                price,
                size,
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
            let source = parts[2].to_string();
            let side = parts[3].to_string();
            let size_str = parts[4].to_string();

            if tk != ticker {
                continue;
            }

            out.push(TradeCsvEvent {
                ts,
                ticker: tk,
                source,
                side,
                size_str,
            });
        }
    }

    out.sort_by_key(|e| e.ts);
    out
}

fn load_ticker_data(base_dir: &str, ticker: &str) -> Option<TickerData> {
    let ob_path = Path::new(base_dir).join(format!("orderbook_{ticker}.csv"));
    let tr_path = Path::new(base_dir).join(format!("trades_{ticker}.csv"));

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

    Some(TickerData {
        ticker: ticker.to_string(),
        book_events,
        trade_events,
        min_ts,
        max_ts,
    })
}

// reconstruct snapshot at target_ts (for replay)
fn compute_snapshot_for(data: &TickerData, target_ts: u64) -> Snapshot {
    let mut bids: BTreeMap<PriceKey, f64> = BTreeMap::new();
    let mut asks: BTreeMap<PriceKey, f64> = BTreeMap::new();

    let mut tf_30s = CandleAgg::new(30);
    let mut tf_1m = CandleAgg::new(60);
    let mut tf_3m = CandleAgg::new(180);
    let mut tf_5m = CandleAgg::new(300);

    for e in &data.book_events {
        if e.ts > target_ts {
            break;
        }

        let map = if e.side.to_lowercase() == "bid" {
            &mut bids
        } else {
            &mut asks
        };

        let key = price_to_key(e.price);

        if e.size == 0.0 {
            map.remove(&key);
        } else {
            map.insert(key, e.size);
        }

        if let (Some((bp, _)), Some((ap, _))) = (bids.iter().next_back(), asks.iter().next()) {
            let mid = (key_to_price(*bp) + key_to_price(*ap)) * 0.5;
            let vol = e.size.abs().max(0.0);

            tf_30s.update(e.ts, mid, vol);
            tf_1m.update(e.ts, mid, vol);
            tf_3m.update(e.ts, mid, vol);
            tf_5m.update(e.ts, mid, vol);
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

    let series_1m = tf_1m.series();
    let (last_mid, last_vol) = if let Some(c) = series_1m.last() {
        (c.close, c.volume)
    } else {
        (0.0, 0.0)
    };

    Snapshot {
        bids,
        asks,
        tf_30s: tf_30s.series().to_vec(),
        tf_1m: tf_1m.series().to_vec(),
        tf_3m: tf_3m.series().to_vec(),
        tf_5m: tf_5m.series().to_vec(),
        last_mid,
        last_vol,
        trades,
    }
}

// NEW: build full CandleAgg history from CSV (for seeding LIVE view)
fn build_candles_from_book_events(
    events: &[BookCsvEvent],
) -> (CandleAgg, CandleAgg, CandleAgg, CandleAgg, u64) {
    let mut bids: BTreeMap<PriceKey, f64> = BTreeMap::new();
    let mut asks: BTreeMap<PriceKey, f64> = BTreeMap::new();

    let mut tf_30s = CandleAgg::new(30);
    let mut tf_1m = CandleAgg::new(60);
    let mut tf_3m = CandleAgg::new(180);
    let mut tf_5m = CandleAgg::new(300);

    let mut last_ts = 0u64;

    for e in events {
        last_ts = e.ts;

        let map = if e.side.to_lowercase() == "bid" {
            &mut bids
        } else {
            &mut asks
        };

        let key = price_to_key(e.price);

        if e.size == 0.0 {
            map.remove(&key);
        } else {
            map.insert(key, e.size);
        }

        if let (Some((bp, _)), Some((ap, _))) = (bids.iter().next_back(), asks.iter().next()) {
            let mid = (key_to_price(*bp) + key_to_price(*ap)) * 0.5;
            let vol = e.size.abs().max(0.0);

            tf_30s.update(e.ts, mid, vol);
            tf_1m.update(e.ts, mid, vol);
            tf_3m.update(e.ts, mid, vol);
            tf_5m.update(e.ts, mid, vol);
        }
    }

    (tf_30s, tf_1m, tf_3m, tf_5m, last_ts)
}

// ------------- crypto provider -------------

fn init_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

// ------------- trade command (real orders) -------------

#[derive(Debug)]
enum TradeCmd {
    MarketOrder {
        ticker: String,
        side: OrderSide,
        size: BigDecimal,
    },
}

// ------------- main app -------------

struct ComboApp {
    // shared
    mode: Mode,
    time_mode: TimeDisplayMode,
    chart: ChartSettings,
    layout: LayoutSettings,
    appearance: AppearanceSettings,
    tickers: Vec<String>,
    current_ticker: String,
    ticker_tx: watch::Sender<String>,

    // live
    live_book_rx: watch::Receiver<LiveBook>,
    live_book: LiveBook,
    live_tf_30s: CandleAgg,
    live_tf_1m: CandleAgg,
    live_tf_3m: CandleAgg,
    live_tf_5m: CandleAgg,
    live_last_ts: u64,

    // real trading
    trade_tx: mpsc::Sender<TradeCmd>,
    trade_size_input: f64,
    last_order_msg: String,

    // replay
    replay_data: HashMap<String, TickerData>,
    replay_ts: u64,
    replay_tab: ReplayTab,
}

impl ComboApp {
    fn new(
        book_rx: watch::Receiver<LiveBook>,
        replay_data: HashMap<String, TickerData>,
        ticker_tx: watch::Sender<String>,
        trade_tx: mpsc::Sender<TradeCmd>,
    ) -> Self {
        let tickers = vec![
            "ETH-USD".to_string(),
            "BTC-USD".to_string(),
            "SOL-USD".to_string(),
        ];

        let current_ticker = "ETH-USD".to_string();

        let replay_ts = replay_data
            .get(&current_ticker)
            .map(|td| td.max_ts)
            .unwrap_or(0);

        // NEW: seed live CandleAggs from CSV history if present
        let (live_tf_30s, live_tf_1m, live_tf_3m, live_tf_5m, live_last_ts) =
            if let Some(td) = replay_data.get(&current_ticker) {
                let (a, b, c, d, last_ts) =
                    build_candles_from_book_events(&td.book_events);
                (a, b, c, d, last_ts)
            } else {
                (
                    CandleAgg::new(30),
                    CandleAgg::new(60),
                    CandleAgg::new(180),
                    CandleAgg::new(300),
                    now_unix(),
                )
            };

        Self {
            mode: Mode::Live,
            time_mode: TimeDisplayMode::Local,
            chart: ChartSettings::default(),
            layout: LayoutSettings::default(),
            appearance: AppearanceSettings::default(),
            tickers,
            current_ticker,
            ticker_tx,

            live_book_rx: book_rx,
            live_book: LiveBook::default(),
            live_tf_30s,
            live_tf_1m,
            live_tf_3m,
            live_tf_5m,
            live_last_ts,

            trade_tx,
            trade_size_input: 0.01,
            last_order_msg: String::new(),

            replay_data,
            replay_ts,
            replay_tab: ReplayTab::Candles,
        }
    }

    fn current_replay_ticker(&self) -> Option<&TickerData> {
        self.replay_data.get(&self.current_ticker)
    }

    fn live_series(&self) -> Vec<Candle> {
        match self.chart.selected_tf {
            30 => self.live_tf_30s.series().to_vec(),
            60 => self.live_tf_1m.series().to_vec(),
            180 => self.live_tf_3m.series().to_vec(),
            300 => self.live_tf_5m.series().to_vec(),
            _ => self.live_tf_1m.series().to_vec(),
        }
    }

    fn replay_series<'a>(&self, snap: &'a Snapshot) -> &'a Vec<Candle> {
        match self.chart.selected_tf {
            30 => &snap.tf_30s,
            60 => &snap.tf_1m,
            180 => &snap.tf_3m,
            300 => &snap.tf_5m,
            _ => &snap.tf_1m,
        }
    }

    fn tick_live(&mut self) {
        if self.live_book_rx.has_changed().unwrap_or(false) {
            self.live_book = self.live_book_rx.borrow().clone();
        }

        let ts = now_unix();
        self.live_last_ts = ts;

        if let Some(mid) = self.live_book.mid() {
            let vol = 0.0; // placeholder volume for now

            self.live_tf_30s.update(ts, mid, vol);
            self.live_tf_1m.update(ts, mid, vol);
            self.live_tf_3m.update(ts, mid, vol);
            self.live_tf_5m.update(ts, mid, vol);
        }
    }

    fn ensure_replay_ts_in_range(&mut self) {
        let (min_ts, max_ts) = match self.replay_data.get(&self.current_ticker) {
            Some(td) => (td.min_ts, td.max_ts),
            None => return,
        };

        if self.replay_ts < min_ts {
            self.replay_ts = min_ts;
        }
        if self.replay_ts > max_ts {
            self.replay_ts = max_ts;
        }
    }

    fn ui_top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            // mode
            ui.label("Mode:");
            if ui
                .selectable_label(self.mode == Mode::Live, "Live")
                .clicked()
            {
                self.mode = Mode::Live;
            }
            if ui
                .selectable_label(self.mode == Mode::Replay, "Replay")
                .clicked()
            {
                self.mode = Mode::Replay;
            }

            ui.separator();

            // ticker menu
            let tickers = self.tickers.clone();
            ui.menu_button(format!("Ticker: {}", self.current_ticker), |ui| {
                for t in &tickers {
                    let selected = *t == self.current_ticker;
                    if ui.selectable_label(selected, t).clicked() {
                        self.current_ticker = t.clone();

                        // notify live feed task
                        let _ = self.ticker_tx.send(t.clone());

                        // adjust replay ts to end of range for that ticker (if exists)
                        if let Some(td) = self.replay_data.get(t) {
                            self.replay_ts = td.max_ts;
                        }

                        ui.close_menu();
                    }
                }
            });

            ui.separator();

            // time display
            ui.label("Time:");
            for mode in [TimeDisplayMode::Local, TimeDisplayMode::Unix] {
                if ui
                    .selectable_label(self.time_mode == mode, mode.label())
                    .clicked()
                {
                    self.time_mode = mode;
                }
            }

            if let Some(td) = self.current_replay_ticker() {
                ui.separator();
                ui.label(format!(
                    "Replay range: {} → {}",
                    format_ts(self.time_mode, td.min_ts),
                    format_ts(self.time_mode, td.max_ts)
                ));
                ui.separator();
                ui.label(format!(
                    "Replay ts: {}",
                    format_ts(self.time_mode, self.replay_ts)
                ));
            }

            if matches!(self.mode, Mode::Live) {
                ui.separator();
                ui.label(format!(
                    "Live ts: {}",
                    format_ts(self.time_mode, self.live_last_ts)
                ));
            }
        });

        ui.separator();

        // replay-only time slider
        if matches!(self.mode, Mode::Replay) {
            if let Some(td) = self.current_replay_ticker() {
                let mut ts = self.replay_ts;
                ui.horizontal(|ui| {
                    ui.label("Replay time:");
                    ui.add(
                        egui::Slider::new(&mut ts, td.min_ts..=td.max_ts)
                            .show_value(false)
                            .text("ts"),
                    );
                    if ui.button("◀").clicked() && ts > td.min_ts {
                        ts -= 1;
                    }
                    if ui.button("▶").clicked() && ts < td.max_ts {
                        ts += 1;
                    }
                    if ui.button("Now").clicked() {
                        ts = td.max_ts;
                    }
                    ui.label(format_ts(self.time_mode, ts));
                });
                self.replay_ts = ts;
            } else {
                ui.label("No replay CSV for this ticker.");
            }

            ui.separator();
        }

        // shared chart controls
        ui.horizontal(|ui| {
            ui.label("History candles:");
            ui.add(
                egui::Slider::new(&mut self.chart.show_candles, 20..=600)
                    .logarithmic(true),
            );

            ui.separator();
            ui.label("X zoom:");
            ui.add(
                egui::Slider::new(&mut self.chart.x_zoom, 0.25..=4.0)
                    .logarithmic(true)
                    .text("zoom"),
            );

            ui.horizontal(|ui| {
                if ui.button("← Pan").clicked() {
                    self.chart.x_pan_secs -= self.chart.selected_tf as f64 * 10.0;
                }
                if ui.button("Pan →").clicked() {
                    self.chart.x_pan_secs += self.chart.selected_tf as f64 * 10.0;
                }
                if ui.button("Center").clicked() {
                    self.chart.x_pan_secs = 0.0;
                }
            });

            ui.separator();
            ui.label("TF:");
            for (label, tf) in [("30s", 30u64), ("1m", 60), ("3m", 180), ("5m", 300)] {
                if ui
                    .selectable_label(self.chart.selected_tf == tf, label)
                    .clicked()
                {
                    self.chart.selected_tf = tf;
                }
            }

            ui.separator();
            ui.checkbox(&mut self.chart.auto_y, "Auto Y");

            if !self.chart.auto_y {
                ui.label("Y range:");
                ui.add(
                    egui::DragValue::new(&mut self.chart.y_min)
                        .speed(1.0)
                        .prefix("min "),
                );
                ui.add(
                    egui::DragValue::new(&mut self.chart.y_max)
                        .speed(1.0)
                        .prefix("max "),
                );
                if ui.button("Reset Y").clicked() {
                    self.chart.auto_y = true;
                }
            }
        });

        ui.separator();

        // Layout & appearance tweaks
        egui::CollapsingHeader::new("Layout & appearance")
            .default_open(false)
            .show(ui, |ui| {
                ui.label("Layout");
                ui.add(
                    egui::Slider::new(
                        &mut self.layout.ladders_height_ratio,
                        0.2..=0.6,
                    )
                    .text("Ladders/trading height"),
                );
                ui.add(
                    egui::Slider::new(&mut self.layout.depth_width_ratio, 0.25..=0.7)
                        .text("Depth width"),
                );
                ui.add(
                    egui::Slider::new(
                        &mut self.layout.volume_height_ratio,
                        0.15..=0.6,
                    )
                    .text("Volume height (vs candles)"),
                );
                ui.add(
                    egui::Slider::new(
                        &mut self.layout.candle_body_width_factor,
                        0.3..=1.0,
                    )
                    .text("Candle body width"),
                );

                ui.separator();
                ui.label("Colors");
                ui.horizontal(|ui| {
                    ui.label("Bull:");
                    ui.color_edit_button_srgba(&mut self.appearance.bull_color);
                    ui.label("Bear:");
                    ui.color_edit_button_srgba(&mut self.appearance.bear_color);
                    ui.label("Volume:");
                    ui.color_edit_button_srgba(&mut self.appearance.volume_color);
                });
            });

        ui.separator();

        // replay sub-tab selector (only in replay mode)
        if matches!(self.mode, Mode::Replay) {
            ui.horizontal(|ui| {
                ui.selectable_value(
                    &mut self.replay_tab,
                    ReplayTab::Orderbook,
                    "Orderbook + Trades",
                );
                ui.selectable_value(
                    &mut self.replay_tab,
                    ReplayTab::Candles,
                    "Candles + Volume",
                );
            });
            ui.separator();
        }
    }

    // ---- LIVE UI ----

    fn ui_live(&mut self, ui: &mut egui::Ui) {
        let series_vec = self.live_series();
        let avail_w = ui.available_width();
        let avail_h = ui.available_height();

        ui.heading(format!("LIVE {}", self.current_ticker));
        ui.separator();

        let ladders_h = avail_h * self.layout.ladders_height_ratio;

        ui.allocate_ui(egui::vec2(avail_w, ladders_h), |ui| {
            let left_w = avail_w * self.layout.depth_width_ratio;
            let right_w = avail_w - left_w;

            ui.horizontal(|ui| {
                // depth
                ui.allocate_ui(egui::vec2(left_w, ladders_h), |ui| {
                    let mut bid_points = Vec::new();
                    let mut ask_points = Vec::new();

                    let mut cum = 0.0;
                    for (k, s) in self.live_book.bids.iter().rev() {
                        let p = key_to_price(*k);
                        cum += s;
                        bid_points.push((p, cum));
                    }

                    cum = 0.0;
                    for (k, s) in self.live_book.asks.iter() {
                        let p = key_to_price(*k);
                        cum += s;
                        ask_points.push((p, cum));
                    }

                    Plot::new("live_depth")
                        .height(ladders_h * 0.9)
                        .show(ui, |plot_ui| {
                            if !bid_points.is_empty() {
                                let pts: PlotPoints = bid_points
                                    .iter()
                                    .map(|(x, y)| [*x, *y])
                                    .collect::<Vec<_>>()
                                    .into();
                                plot_ui.line(Line::new(pts).name("Bids"));
                            }
                            if !ask_points.is_empty() {
                                let pts: PlotPoints = ask_points
                                    .iter()
                                    .map(|(x, y)| [*x, *y])
                                    .collect::<Vec<_>>()
                                    .into();
                                plot_ui.line(Line::new(pts).name("Asks"));
                            }
                        });
                });

                ui.separator();

                // ladders + trading (scrollable vertically)
                ui.allocate_ui(egui::vec2(right_w, ladders_h), |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.label("Live ladders (top 20)");

                            ui.columns(2, |cols| {
                                cols[0].label("Bids");
                                egui::Grid::new("live_bids_grid")
                                    .striped(true)
                                    .show(&mut cols[0], |ui| {
                                        ui.label("Price");
                                        ui.label("Size");
                                        ui.end_row();
                                        for (k, s) in
                                            self.live_book.bids.iter().rev().take(20)
                                        {
                                            let p = key_to_price(*k);
                                            ui.label(format!("{:>9.2}", p));
                                            ui.label(format!("{:>8.4}", s));
                                            ui.end_row();
                                        }
                                    });

                                cols[1].label("Asks");
                                egui::Grid::new("live_asks_grid")
                                    .striped(true)
                                    .show(&mut cols[1], |ui| {
                                        ui.label("Price");
                                        ui.label("Size");
                                        ui.end_row();
                                        for (k, s) in
                                            self.live_book.asks.iter().take(20)
                                        {
                                            let p = key_to_price(*k);
                                            ui.label(format!("{:>9.2}", p));
                                            ui.label(format!("{:>8.4}", s));
                                            ui.end_row();
                                        }
                                    });
                            });

                            ui.separator();

                            ui.group(|ui| {
                                ui.heading("Real testnet market order");

                                ui.label("Requires DYDX_TESTNET_MNEMONIC in your shell.");

                                ui.horizontal(|ui| {
                                    ui.label("Size (units):");
                                    ui.add(
                                        egui::DragValue::new(
                                            &mut self.trade_size_input,
                                        )
                                        .speed(0.001)
                                        .clamp_range(0.0..=1000.0),
                                    );
                                });

                                ui.horizontal(|ui| {
                                    if ui.button("Market BUY").clicked() {
                                        let size_val =
                                            self.trade_size_input.max(0.0);
                                        let s_str =
                                            format!("{:.8}", size_val);
                                        if let Ok(size_bd) =
                                            BigDecimal::from_str(&s_str)
                                        {
                                            let _ = self
                                                .trade_tx
                                                .try_send(TradeCmd::MarketOrder {
                                                    ticker: self
                                                        .current_ticker
                                                        .clone(),
                                                    side: OrderSide::Buy,
                                                    size: size_bd,
                                                });
                                            self.last_order_msg =
                                                format!(
                                                "Sent BUY {} size {} (check terminal + trades CSV)",
                                                self.current_ticker, s_str
                                            );
                                        } else {
                                            self.last_order_msg =
                                                "Invalid size for BUY"
                                                    .to_string();
                                        }
                                    }
                                    if ui.button("Market SELL").clicked() {
                                        let size_val =
                                            self.trade_size_input.max(0.0);
                                        let s_str =
                                            format!("{:.8}", size_val);
                                        if let Ok(size_bd) =
                                            BigDecimal::from_str(&s_str)
                                        {
                                            let _ = self
                                                .trade_tx
                                                .try_send(TradeCmd::MarketOrder {
                                                    ticker: self
                                                        .current_ticker
                                                        .clone(),
                                                    side: OrderSide::Sell,
                                                    size: size_bd,
                                                });
                                            self.last_order_msg =
                                                format!(
                                                "Sent SELL {} size {} (check terminal + trades CSV)",
                                                self.current_ticker, s_str
                                            );
                                        } else {
                                            self.last_order_msg =
                                                "Invalid size for SELL"
                                                    .to_string();
                                        }
                                    }
                                });

                                if !self.last_order_msg.is_empty() {
                                    ui.separator();
                                    ui.label(&self.last_order_msg);
                                }
                            });
                        });
                });
            });
        });

        ui.separator();

        self.ui_candles_generic(ui, &series_vec, None, true);
    }

    // ---- REPLAY UI ----

    fn ui_replay(&mut self, ui: &mut egui::Ui) {
        self.ensure_replay_ts_in_range();

        let snapshot = self
            .current_replay_ticker()
            .map(|td| compute_snapshot_for(td, self.replay_ts));

        if snapshot.is_none() {
            ui.heading("No replay data for this ticker.");
            ui.label("Make sure CSVs exist in ./data.");
            return;
        }

        let snap = snapshot.as_ref().unwrap();

        match self.replay_tab {
            ReplayTab::Orderbook => self.ui_replay_orderbook(ui, snap),
            ReplayTab::Candles => {
                let series_vec = self.replay_series(snap).clone();
                self.ui_candles_generic(ui, &series_vec, Some(snap), false);
            }
        }
    }

    fn ui_replay_orderbook(&self, ui: &mut egui::Ui, snap: &Snapshot) {
        ui.heading(format!(
            "REPLAY {} @ {}",
            self.current_ticker,
            format_ts(self.time_mode, self.replay_ts)
        ));

        let avail_w = ui.available_width();
        let avail_h = ui.available_height();
        let depth_w = avail_w * self.layout.depth_width_ratio;
        let ladders_w = avail_w - depth_w;

        ui.horizontal(|ui| {
            // depth
            ui.allocate_ui(egui::vec2(depth_w, avail_h), |ui| {
                let mut bid_points = Vec::new();
                let mut ask_points = Vec::new();

                let mut cum = 0.0;
                for (k, s) in snap.bids.iter().rev() {
                    let p = key_to_price(*k);
                    cum += s;
                    bid_points.push((p, cum));
                }

                cum = 0.0;
                for (k, s) in snap.asks.iter() {
                    let p = key_to_price(*k);
                    cum += s;
                    ask_points.push((p, cum));
                }

                Plot::new("replay_depth")
                    .height(avail_h * 0.9)
                    .show(ui, |plot_ui| {
                        if !bid_points.is_empty() {
                            let pts: PlotPoints = bid_points
                                .iter()
                                .map(|(x, y)| [*x, *y])
                                .collect::<Vec<_>>()
                                .into();
                            plot_ui.line(Line::new(pts).name("Bids"));
                        }
                        if !ask_points.is_empty() {
                            let pts: PlotPoints = ask_points
                                .iter()
                                .map(|(x, y)| [*x, *y])
                                .collect::<Vec<_>>()
                                .into();
                            plot_ui.line(Line::new(pts).name("Asks"));
                        }
                    });
            });

            ui.separator();

            // ladders + trades
            ui.allocate_ui(egui::vec2(ladders_w, avail_h), |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.label("Snapshot ladders");

                        ui.columns(2, |cols| {
                            cols[0].label("Bids");
                            egui::Grid::new("replay_bids_grid")
                                .striped(true)
                                .show(&mut cols[0], |ui| {
                                    ui.label("Price");
                                    ui.label("Size");
                                    ui.end_row();
                                    for (k, s) in
                                        snap.bids.iter().rev().take(20)
                                    {
                                        let p = key_to_price(*k);
                                        ui.label(format!("{:>9.2}", p));
                                        ui.label(format!("{:>8.4}", s));
                                        ui.end_row();
                                    }
                                });

                            cols[1].label("Asks");
                            egui::Grid::new("replay_asks_grid")
                                .striped(true)
                                .show(&mut cols[1], |ui| {
                                    ui.label("Price");
                                    ui.label("Size");
                                    ui.end_row();
                                    for (k, s) in
                                        snap.asks.iter().take(20)
                                    {
                                        let p = key_to_price(*k);
                                        ui.label(format!("{:>9.2}", p));
                                        ui.label(format!("{:>8.4}", s));
                                        ui.end_row();
                                    }
                                });
                        });

                        ui.separator();
                        ui.label(format!(
                            "Last mid: {:.2}   Last vol: {:.4}",
                            snap.last_mid, snap.last_vol
                        ));

                        ui.separator();
                        ui.label("Recent trades:");
                        egui::ScrollArea::vertical()
                            .max_height(avail_h * 0.4)
                            .show(ui, |ui| {
                                egui::Grid::new("replay_trades_grid")
                                    .striped(true)
                                    .show(ui, |ui| {
                                        ui.label("Time");
                                        ui.label("Side");
                                        ui.label("Size");
                                        ui.end_row();

                                        for tr in snap.trades.iter().rev() {
                                            ui.label(format_ts(
                                                self.time_mode, tr.ts,
                                            ));
                                            ui.label(&tr.side);
                                            ui.label(&tr.size_str);
                                            ui.end_row();
                                        }
                                    });
                            });
                    });
            });
        });
    }

    // ---- generic candles+volume for live & replay ----

    fn ui_candles_generic(
        &mut self,
        ui: &mut egui::Ui,
        series_vec: &Vec<Candle>,
        _snap: Option<&Snapshot>,
        is_live: bool,
    ) {
        if series_vec.is_empty() {
            ui.label(if is_live {
                "No live candles yet (waiting for book mid)..."
            } else {
                "No candles yet at this replay time."
            });
            return;
        }

        let len = series_vec.len();
        let window_len = self.chart.show_candles.min(len).max(1);
        let visible = &series_vec[len - window_len..];

        let (y_min, y_max) = if self.chart.auto_y {
            let lo = visible.iter().map(|c| c.low).fold(f64::MAX, f64::min);
            let hi = visible.iter().map(|c| c.high).fold(f64::MIN, f64::max);
            let span = (hi - lo).max(1e-3);
            let pad = span * 0.05;
            let min_v = lo - pad;
            let max_v = hi + pad;
            self.chart.y_min = min_v;
            self.chart.y_max = max_v;
            (min_v, max_v)
        } else {
            (self.chart.y_min, self.chart.y_max)
        };

        let avail_h = ui.available_height();
        let avail_w = ui.available_width();

        let volume_ratio = self.layout.volume_height_ratio.clamp(0.05, 0.8);
        let candles_h = avail_h * (1.0 - volume_ratio);
        let volume_h = avail_h * volume_ratio;

        let tf = self.chart.selected_tf as f64;
        let last = visible.last().unwrap();
        let x_center = last.t as f64 + tf * 0.5;
        let base_span = tf * self.chart.show_candles as f64;
        let span = base_span / self.chart.x_zoom.max(1e-6);
        let x_min = x_center - span * 0.5 + self.chart.x_pan_secs;
        let x_max = x_center + span * 0.5 + self.chart.x_pan_secs;

        // candles
        ui.allocate_ui(egui::vec2(avail_w, candles_h), |ui| {
            let mode = self.time_mode;
            let bull = self.appearance.bull_color;
            let bear = self.appearance.bear_color;
            let body_factor = self.layout.candle_body_width_factor.clamp(0.1, 1.2);

            let plot_resp = Plot::new(if is_live {
                "candles_live"
            } else {
                "candles_replay"
            })
            .height(candles_h)
            .include_y(y_min)
            .include_y(y_max)
            .allow_drag(true)
            .allow_zoom(true)
            .x_axis_formatter(move |mark, _bounds, _transform| {
                let ts = mark.value as u64;
                format_ts(mode, ts)
            })
            .show(ui, |plot_ui| {
                plot_ui.set_plot_bounds(PlotBounds::from_min_max(
                    [x_min, y_min],
                    [x_max, y_max],
                ));

                for c in visible {
                    let left = c.t as f64;
                    let right = left + tf;
                    let mid = left + tf * 0.5;

                    let top = c.open.max(c.close);
                    let bot = c.open.min(c.close);

                    let color = if c.close >= c.open { bull } else { bear };

                    // wick
                    let wick_pts: PlotPoints =
                        vec![[mid, c.low], [mid, c.high]].into();
                    plot_ui.line(Line::new(wick_pts).color(color));

                    // body width relative to TF
                    let half_body = (tf * 0.5 * body_factor as f64).min(tf * 0.5);
                    let body_left = mid - half_body;
                    let body_right = mid + half_body;

                    // filled body polygon
                    let body_pts: PlotPoints = vec![
                        [body_left, bot],
                        [body_left, top],
                        [body_right, top],
                        [body_right, bot],
                        [body_left, bot],
                    ]
                    .into();
                    plot_ui.line(Line::new(body_pts).color(color).width(2.0));
                }

                let now_x = if is_live {
                    self.live_last_ts as f64
                } else {
                    self.replay_ts as f64
                };
                plot_ui.vline(VLine::new(now_x).name("now_ts"));
            });

            // vertical zoom: Shift + scroll over candles plot
            let hovered = plot_resp.response.hovered();
            let mut scroll_y = 0.0f32;
            let mut shift = false;
            ui.ctx().input(|i| {
                scroll_y = i.raw_scroll_delta.y;
                shift = i.modifiers.shift;
            });
            if hovered && shift && scroll_y != 0.0 {
                self.chart.auto_y = false;
                let factor = 1.0 + (scroll_y as f64 * 0.002); // smooth
                let factor = factor.clamp(0.2, 5.0);
                let center = (self.chart.y_min + self.chart.y_max) * 0.5;
                let half_span =
                    (self.chart.y_max - self.chart.y_min).max(1e-6) * factor * 0.5;
                self.chart.y_min = center - half_span;
                self.chart.y_max = center + half_span;
            }
        });

        ui.separator();

        // volume
        ui.allocate_ui(egui::vec2(avail_w, volume_h), |ui| {
            let mode = self.time_mode;
            let vol_color = self.appearance.volume_color;

            let plot_resp = Plot::new(if is_live {
                "volume_live"
            } else {
                "volume_replay"
            })
            .height(volume_h)
            .include_y(0.0)
            .allow_drag(true)
            .allow_zoom(true)
            .x_axis_formatter(move |mark, _bounds, _transform| {
                let ts = mark.value as u64;
                format_ts(mode, ts)
            })
            .show(ui, |plot_ui| {
                let max_vol = visible
                    .iter()
                    .map(|c| c.volume)
                    .fold(0.0_f64, f64::max)
                    .max(1e-6);
                let y_max_v = max_vol * 1.1;

                plot_ui.set_plot_bounds(PlotBounds::from_min_max(
                    [x_min, 0.0],
                    [x_max, y_max_v],
                ));

                for c in visible {
                    let left = c.t as f64;
                    let mid = left + tf * 0.5;

                    let line_pts: PlotPoints =
                        vec![[mid, 0.0], [mid, c.volume]].into();
                    plot_ui
                        .line(Line::new(line_pts).color(vol_color).width(2.0));
                }
            });

            // vertical zoom also works on volume (Shift + scroll)
            let hovered = plot_resp.response.hovered();
            let mut scroll_y = 0.0f32;
            let mut shift = false;
            ui.ctx().input(|i| {
                scroll_y = i.raw_scroll_delta.y;
                shift = i.modifiers.shift;
            });
            if hovered && shift && scroll_y != 0.0 {
                self.chart.auto_y = false;
                let factor = 1.0 + (scroll_y as f64 * 0.002);
                let factor = factor.clamp(0.2, 5.0);
                let center = (self.chart.y_min + self.chart.y_max) * 0.5;
                let half_span =
                    (self.chart.y_max - self.chart.y_min).max(1e-6) * factor * 0.5;
                self.chart.y_min = center - half_span;
                self.chart.y_max = center + half_span;
            }
        });
    }
}

impl eframe::App for ComboApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if matches!(self.mode, Mode::Live) {
            self.tick_live();
        }

        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            self.ui_top_bar(ui);
        });

        egui::CentralPanel::default().show(ctx, |ui| match self.mode {
            Mode::Live => self.ui_live(ui),
            Mode::Replay => self.ui_replay(ui),
        });

        ctx.request_repaint_after(Duration::from_millis(50));
    }
}

// ------------- async live feed -------------

async fn run_live_feed(book_tx: watch::Sender<LiveBook>, ticker_rx: watch::Receiver<String>) {
    let config = match ClientConfig::from_file("client/tests/testnet.toml").await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to load testnet.toml: {e}");
            return;
        }
    };

    let mut indexer = IndexerClient::new(config.indexer);
    let mut ticker_rx = ticker_rx;

    loop {
        let current = ticker_rx.borrow().clone();
        eprintln!("Subscribing live feed for {current}");

        let mut feeds: Feeds<'_> = indexer.feed();
        let ticker = Ticker(current.clone());

        let mut feed: DxFeed<OrdersMessage> = match feeds.orders(&ticker, false).await {
            Ok(f) => f,
            Err(e) => {
                eprintln!("orders feed error for {current}: {e}");
                return;
            }
        };

        let mut book = LiveBook::default();

        while let Some(msg) = feed.recv().await {
            match msg {
                OrdersMessage::Initial(init) => {
                    book.apply_initial(init.contents.bids, init.contents.asks, &current);
                }
                OrdersMessage::Update(upd) => {
                    book.apply_update(upd.contents.bids, upd.contents.asks, &current);
                }
            }
            let _ = book_tx.send(book.clone());

            if ticker_rx.has_changed().unwrap_or(false) {
                break;
            }
        }
    }
}

// ------------- async trade executor (real orders) -------------

async fn run_trader(mut rx: mpsc::Receiver<TradeCmd>) {
    let config = match ClientConfig::from_file("client/tests/testnet.toml").await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[trader] Failed to load testnet.toml: {e}");
            return;
        }
    };

    let raw = match env::var("DYDX_TESTNET_MNEMONIC") {
        Ok(v) => v,
        Err(_) => {
            eprintln!("[trader] DYDX_TESTNET_MNEMONIC not set; trading disabled");
            return;
        }
    };
    let mnemonic = raw.split_whitespace().collect::<Vec<_>>().join(" ");

    let wallet = match Wallet::from_mnemonic(&mnemonic) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("[trader] invalid mnemonic: {e}");
            return;
        }
    };

    let mut node = match NodeClient::connect(config.node).await {
        Ok(n) => n,
        Err(e) => {
            eprintln!("[trader] node connect failed: {e}");
            return;
        }
    };

    let mut account = match wallet.account(0, &mut node).await {
        Ok(a) => a,
        Err(e) => {
            eprintln!("[trader] account sync failed: {e}");
            return;
        }
    };

    let sub = match account.subaccount(0) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[trader] subaccount derive failed: {e}");
            return;
        }
    };

    let indexer = IndexerClient::new(config.indexer);

    while let Some(cmd) = rx.recv().await {
        match cmd {
            TradeCmd::MarketOrder { ticker, side, size } => {
                eprintln!("[trader] market {:?} {} size {}", side, ticker, size);

                let market = match indexer
                    .markets()
                    .get_perpetual_market(&ticker.clone().into())
                    .await
                {
                    Ok(m) => m,
                    Err(e) => {
                        eprintln!("[trader] market meta error for {ticker}: {e}");
                        continue;
                    }
                };

                let h = match node.latest_block_height().await {
                    Ok(h) => h,
                    Err(e) => {
                        eprintln!("[trader] height error: {e}");
                        continue;
                    }
                };

                let (_id, order) = match OrderBuilder::new(market, sub.clone())
                    .market(side, size.clone())
                    .reduce_only(false)
                    .price(100) // placeholder slippage guard; adjust later
                    .time_in_force(TimeInForce::Unspecified)
                    .until(h.ahead(10))
                    .build(123456)
                {
                    Ok(x) => x,
                    Err(e) => {
                        eprintln!("[trader] build order error: {e}");
                        continue;
                    }
                };

                match node.place_order(&mut account, order).await {
                    Ok(tx_hash) => {
                        eprintln!(
                            "[trader] placed {:?} {} size {} tx={tx_hash:?}",
                            side, ticker, size
                        );
                        append_trade_csv(
                            &ticker,
                            "gui_live",
                            &format!("{:?}", side),
                            &size.to_string(),
                        );
                    }
                    Err(e) => {
                        eprintln!("[trader] place_order error: {e}");
                    }
                }
            }
        }
    }
}

// ------------- main -------------

fn main() {
    init_crypto_provider();

    let (book_tx, book_rx) = watch::channel(LiveBook::default());

    // preload replay data from ./data
    let base_dir = "data";
    let tickers = vec!["ETH-USD", "BTC-USD", "SOL-USD"];
    let mut replay_data = HashMap::new();
    for tk in tickers {
        if let Some(td) = load_ticker_data(base_dir, tk) {
            replay_data.insert(tk.to_string(), td);
        }
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let (ticker_tx, ticker_rx) =
        watch::channel::<String>("ETH-USD".to_string());

    let (trade_tx, trade_rx) = mpsc::channel::<TradeCmd>(32);

    // spawn live feed
    rt.spawn(run_live_feed(book_tx, ticker_rx));

    // spawn trader
    rt.spawn(run_trader(trade_rx));

    let options = eframe::NativeOptions::default();
    let app = ComboApp::new(book_rx, replay_data, ticker_tx.clone(), trade_tx);

    if let Err(e) = eframe::run_native(
        "dYdX Live + Replay Combo",
        options,
        Box::new(|_cc| Box::new(app)),
    ) {
        eprintln!("eframe error: {e}");
    }

    drop(rt);
}
