// ladder_app/src/bin/full_gui_x14.rs
//
// GUI for dYdX v4 using CSVs written by the background daemon:
//
//  - Daemon writes under ./data:
//      data/orderbook_{TICKER}.csv
//      data/trades_{TICKER}.csv
//
//  - This GUI:
//      * Reads those CSVs on a timer (configurable via UI / script).
//      * Reconstructs orderbook snapshot + candles from midprice.
//      * Shows depth plot, ladders, recent trades.
//      * Candles + volume, TFs from 1s → 1d.
//      * 3×6 grid layout, per-row height + horizontal span modes,
//        scrollable page.
//      * Rhai script engine for a simple “bot”, fed orderbook metrics.
//      * Trading panel: market/“limit” (price guard), size, leverage,
//        bot suggestions.
//
//  Requirements:
//    - Data daemon already running and writing CSVs regularly.
//    - client/tests/testnet.toml present for dYdX client config.
//    - DYDX_TESTNET_MNEMONIC exported if you want real trades.
//
//  Build:
//    cargo run --release -p ladder_app --bin full_gui_x14
//

mod candle_agg;

use candle_agg::{Candle, CandleAgg};

use chrono::{Local, TimeZone};

use eframe::egui::{self, Color32};
use egui_plot::{Line, Plot, PlotBounds, PlotPoints};

use std::cmp::{max, min};
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
use dydx_proto::dydxprotocol::clob::order::TimeInForce;

// ---------- basic helpers ----------

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_secs()
}

// We key prices in BTreeMap as scaled integers for nice ordering.
type PriceKey = i64;

fn price_to_key(price: f64) -> PriceKey {
    (price * 10_000.0).round() as PriceKey
}

fn key_to_price(key: PriceKey) -> f64 {
    key as f64 / 10_000.0
}

// ---------- time display mode ----------

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

// ---------- chart settings ----------

#[derive(Clone)]
struct ChartSettings {
    // how many candles visible in window
    show_candles: usize,
    auto_y: bool,
    y_min: f64,
    y_max: f64,
    x_zoom: f64,
    x_pan_secs: f64,
    tf_secs: u64,
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

// ---------- modes / layout ----------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Live,
    Replay,
}

const GRID_ROWS: usize = 6;
const GRID_COLS: usize = 3;

#[derive(Clone, Copy, PartialEq, Eq)]
enum RowSpanMode {
    Split3,       // 3 equal columns
    Left2Right1,  // col0 spans 2/3, col1 is 1/3, col2 empty
    Left1Right2,  // col0 1/3, col1 2/3, col2 empty
    Full,         // one full-width cell
}

#[derive(Copy, Clone)]
struct RowConfig {
    height_factor: f32, // 0.5..3.0
    span_mode: RowSpanMode,
    big_ratio: f32, // for 2+1 / 1+2 rows, fraction for big side (0.3..0.8)
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

// ---------- CSV + replay structures ----------

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
    bids: BTreeMap<PriceKey, f64>,
    asks: BTreeMap<PriceKey, f64>,
    candles: Vec<Candle>,
    trades: Vec<TradeCsvEvent>,
    last_mid: f64,
    last_vol: f64,
}

// ---------- CSV I/O ----------

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
            if tk != ticker {
                continue;
            }
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

    Some(TickerData {
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

        let key = price_to_key(e.price);

        if e.size == 0.0 {
            map.remove(&key);
        } else {
            map.insert(key, e.size);
        }

        if let (Some((bp, _)), Some((ap, _))) = (bids.iter().next_back(), asks.iter().next()) {
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
        (c.close, c.volume)
    } else {
        (0.0, 0.0)
    };

    Snapshot {
        bids,
        asks,
        candles: series,
        trades,
        last_mid,
        last_vol,
    }
}

// ---------- crypto provider ----------

fn init_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

// ---------- trading ----------

#[derive(Clone, Debug)]
enum TradeKind {
    Market,
    Limit,
}

#[derive(Debug)]
struct TradeCmd {
    ticker: String,
    side: OrderSide,
    size: BigDecimal,
    kind: TradeKind,
    limit_price: f64,
    leverage: f64,
}

// ---------- main app state ----------

struct ComboApp {
    // data & mode
    base_dir: PathBuf,
    ticker_data: HashMap<String, TickerData>,
    tickers: Vec<String>,
    current_ticker: String,
    mode: Mode,
    time_mode: TimeDisplayMode,

    // time & reload
    live_ts: u64,
    replay_ts: u64,
    last_reload_ts: u64,
    reload_secs: f64,

    // chart
    chart: ChartSettings,
    show_depth: bool,
    show_ladders: bool,
    show_trades: bool,
    show_volume: bool,

    // layout: 3x6
    row_cfgs: [RowConfig; GRID_ROWS],

    // script engine
    engine: Engine,
    scope: Scope<'static>,
    script_text: String,
    script_last_error: Option<String>,
    script_auto_run: bool,
    script_last_run_ts: u64,

    // bot results
    bot_signal: String,
    bot_size: f64,
    bot_comment: String,
    bot_auto_trade: bool,
    bot_last_executed_signal: String,

    // trading UI
    trade_side: OrderSide,
    trade_kind: TradeKind,
    trade_size_units: f64,
    trade_leverage: f64,
    trade_limit_price: f64,
    last_order_msg: String,
    trade_tx: mpsc::Sender<TradeCmd>,
}

impl ComboApp {
    fn new(
        base_dir: PathBuf,
        ticker_data: HashMap<String, TickerData>,
        tickers: Vec<String>,
        trade_tx: mpsc::Sender<TradeCmd>,
    ) -> Self {
        let current_ticker = tickers
            .get(0)
            .cloned()
            .unwrap_or_else(|| "ETH-USD".to_string());

        let (live_ts, replay_ts) = ticker_data
            .get(&current_ticker)
            .map(|td| (td.max_ts, td.max_ts))
            .unwrap_or((now_unix(), now_unix()));

        let mut row_cfgs: [RowConfig; GRID_ROWS] = [RowConfig::default(); GRID_ROWS];
        // Example defaults:
        // row 0: big full-width chart
        row_cfgs[0].span_mode = RowSpanMode::Full;
        row_cfgs[0].height_factor = 2.0;
        // row 1: depth / ladders / trading
        row_cfgs[1].span_mode = RowSpanMode::Split3;
        row_cfgs[1].height_factor = 1.4;
        // row 2: script + bot status + recent trades
        row_cfgs[2].span_mode = RowSpanMode::Split3;
        row_cfgs[2].height_factor = 1.4;
        // others: leave default

        let mut engine = Engine::new();
        engine.set_max_expr_depths(64, 64);

        let mut scope = Scope::new();

        let default_script = r#"
// Rhai bot script.
// Inputs (set by Rust):
//   ticker:            String
//   mode:              "live" | "replay"
//   best_bid, best_ask, mid, spread: f64
//   bid_liquidity_near, ask_liquidity_near: f64
//   tf_secs, history_candles: i64
//
// Outputs (you MUST set these):
//   bot_signal  = "none" | "buy" | "sell"
//   bot_size    = positive float (units)
//   bot_comment = string

let imbalance = if ask_liquidity_near > 0.0 {
    bid_liquidity_near / ask_liquidity_near
} else {
    0.0
};

bot_signal = "none";
bot_size = 0.0;
bot_comment = "";

if imbalance > 2.5 && spread < mid * 0.0005 {
    bot_signal = "buy";
    bot_size = 0.01;
    bot_comment = "Bid bubble detected";
} else if imbalance < 0.4 && spread < mid * 0.0005 {
    bot_signal = "sell";
    bot_size = 0.01;
    bot_comment = "Ask bubble detected";
}
"#.to_string();

        scope.set_value("bot_signal", "none".to_string());
        scope.set_value("bot_size", 0.0_f64);
        scope.set_value("bot_comment", "".to_string());

        Self {
            base_dir,
            ticker_data,
            tickers,
            current_ticker,
            mode: Mode::Live,
            time_mode: TimeDisplayMode::Local,

            live_ts,
            replay_ts,
            last_reload_ts: now_unix(),
            reload_secs: 5.0,

            chart: ChartSettings::default(),
            show_depth: true,
            show_ladders: true,
            show_trades: true,
            show_volume: true,

            row_cfgs,

            engine,
            scope,
            script_text: default_script,
            script_last_error: None,
            script_auto_run: true,
            script_last_run_ts: 0,

            bot_signal: "none".to_string(),
            bot_size: 0.0,
            bot_comment: String::new(),
            bot_auto_trade: false,
            bot_last_executed_signal: "none".to_string(),

            trade_side: OrderSide::Buy,
            trade_kind: TradeKind::Market,
            trade_size_units: 0.01,
            trade_leverage: 5.0,
            trade_limit_price: 0.0,
            last_order_msg: String::new(),
            trade_tx,
        }
    }

    fn ticker_range(&self) -> Option<(u64, u64)> {
        self.ticker_data
            .get(&self.current_ticker)
            .map(|td| (td.min_ts, td.max_ts))
    }

    fn reload_current_ticker(&mut self) {
        if let Some(td) = load_ticker_data(&self.base_dir, &self.current_ticker) {
            self.live_ts = td.max_ts;
            if self.replay_ts < td.min_ts || self.replay_ts > td.max_ts {
                self.replay_ts = td.max_ts;
            }
            self.ticker_data.insert(self.current_ticker.clone(), td);
        }
    }

    fn clamp_ts_to_range(&mut self) {
        if let Some((min_ts, max_ts)) = self.ticker_range() {
            if self.live_ts < min_ts || self.live_ts > max_ts {
                self.live_ts = max_ts;
            }
            if self.replay_ts < min_ts {
                self.replay_ts = min_ts;
            }
            if self.replay_ts > max_ts {
                self.replay_ts = max_ts;
            }
        }
    }

    fn current_snap_live(&self) -> Option<Snapshot> {
        let tf = self.chart.tf_secs;
        let td = self.ticker_data.get(&self.current_ticker)?;
        let live_ts = td.max_ts;
        Some(compute_snapshot_for(td, live_ts, tf))
    }

    fn current_snap_replay(&self) -> Option<Snapshot> {
        let tf = self.chart.tf_secs;
        let td = self.ticker_data.get(&self.current_ticker)?;
        Some(compute_snapshot_for(td, self.replay_ts, tf))
    }

    fn current_snap(&self) -> Option<Snapshot> {
        match self.mode {
            Mode::Live => self.current_snap_live(),
            Mode::Replay => self.current_snap_replay(),
        }
    }

    // ---------- bot + script ----------

    fn feed_scope_from_snapshot(&mut self, snap: &Snapshot) {
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
        for (_, s) in snap.bids.iter().rev().take(10) {
            bid_liq += *s;
        }
        let mut ask_liq = 0.0;
        for (_, s) in snap.asks.iter().take(10) {
            ask_liq += *s;
        }

        self.scope.clear();

        self.scope
            .set_value("ticker", self.current_ticker.clone());
        self.scope.set_value(
            "mode",
            match self.mode {
                Mode::Live => "live".to_string(),
                Mode::Replay => "replay".to_string(),
            },
        );
        self.scope.set_value("best_bid", best_bid);
        self.scope.set_value("best_ask", best_ask);
        self.scope.set_value("mid", mid);
        self.scope.set_value("spread", spread);
        self.scope
            .set_value("bid_liquidity_near", bid_liq);
        self.scope
            .set_value("ask_liquidity_near", ask_liq);
        self.scope
            .set_value("tf_secs", self.chart.tf_secs as i64);
        self.scope
            .set_value("history_candles", self.chart.show_candles as i64);

        self.scope
            .set_value("bot_signal", self.bot_signal.clone());
        self.scope.set_value("bot_size", self.bot_size);
        self.scope
            .set_value("bot_comment", self.bot_comment.clone());
    }

    fn read_bot_from_scope(&mut self) {
        if let Some(sig) = self.scope.get_value::<String>("bot_signal") {
            self.bot_signal = sig;
        } else {
            self.bot_signal = "none".to_string();
        }
        if let Some(size) = self.scope.get_value::<f64>("bot_size") {
            self.bot_size = size.max(0.0);
        } else {
            self.bot_size = 0.0;
        }
        if let Some(comment) = self.scope.get_value::<String>("bot_comment") {
            self.bot_comment = comment;
        } else {
            self.bot_comment.clear();
        }
    }

    fn run_script(&mut self, snap: &Snapshot) {
        self.script_last_error = None;

        self.feed_scope_from_snapshot(snap);

        let res = self
            .engine
            .eval_with_scope::<()>(
                &mut self.scope,
                &self.script_text,
            );

        match res {
            Ok(()) => {
                self.read_bot_from_scope();

                if self.bot_auto_trade
                    && (self.bot_signal == "buy"
                        || self.bot_signal == "sell")
                    && self.bot_signal != self.bot_last_executed_signal
                    && self.bot_size > 0.0
                {
                    let maybe_side = match self.bot_signal.as_str() {
                        "buy" => Some(OrderSide::Buy),
                        "sell" => Some(OrderSide::Sell),
                        _ => None,
                    };
                    if let Some(side) = maybe_side {
                        let size_str =
                            format!("{:.8}", self.bot_size.max(0.0));
                        if let Ok(size_bd) = BigDecimal::from_str(&size_str) {
                            let cmd = TradeCmd {
                                ticker: self.current_ticker.clone(),
                                side,
                                size: size_bd,
                                kind: TradeKind::Market,
                                limit_price: 0.0,
                                leverage: self.trade_leverage,
                            };
                            let _ = self.trade_tx.try_send(cmd);
                            self.last_order_msg = format!(
                                "[BOT] auto {:?} {} size {}",
                                side, self.current_ticker, size_str
                            );
                            self.bot_last_executed_signal =
                                self.bot_signal.clone();
                            append_trade_csv(
                                &self.current_ticker,
                                "bot_auto",
                                &format!("{:?}", side),
                                &size_str,
                            );
                        }
                    }
                }
            }
            Err(e) => {
                self.script_last_error = Some(e.to_string());
            }
        }

        self.script_last_run_ts = now_unix();
    }

    // ---------- UI pieces ----------

    fn ui_top_bar(&mut self, ui: &mut egui::Ui, snap_opt: Option<&Snapshot>) {
        ui.horizontal(|ui| {
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

            let tickers = self.tickers.clone();
            ui.menu_button(
                format!("Ticker: {}", self.current_ticker),
                |ui| {
                    for t in &tickers {
                        let selected = *t == self.current_ticker;
                        if ui.selectable_label(selected, t).clicked() {
                            self.current_ticker = t.clone();
                            if let Some((_, max_ts)) = self.ticker_range() {
                                self.live_ts = max_ts;
                                self.replay_ts = max_ts;
                            }
                            ui.close_menu();
                        }
                    }
                },
            );

            ui.separator();

            ui.label("Time:");
            for mode in [TimeDisplayMode::Local, TimeDisplayMode::Unix] {
                if ui
                    .selectable_label(self.time_mode == mode, mode.label())
                    .clicked()
                {
                    self.time_mode = mode;
                }
            }

            if let Some((min_ts, max_ts)) = self.ticker_range() {
                ui.separator();
                ui.label(format!(
                    "Range: {} → {}",
                    format_ts(self.time_mode, min_ts),
                    format_ts(self.time_mode, max_ts)
                ));
                if matches!(self.mode, Mode::Replay) {
                    ui.separator();
                    ui.label(format!(
                        "Replay @ {}",
                        format_ts(self.time_mode, self.replay_ts)
                    ));
                } else {
                    ui.separator();
                    ui.label(format!(
                        "Live @ {}",
                        format_ts(self.time_mode, max_ts)
                    ));
                }
            }
        });

        if matches!(self.mode, Mode::Replay) {
            if let Some((min_ts, max_ts)) = self.ticker_range() {
                let mut ts = self.replay_ts;
                ui.horizontal(|ui| {
                    ui.label("Replay time:");
                    ui.add(
                        egui::Slider::new(&mut ts, min_ts..=max_ts)
                            .show_value(false),
                    );
                    if ui.button("◀").clicked() && ts > min_ts {
                        ts -= 1;
                    }
                    if ui.button("▶").clicked() && ts < max_ts {
                        ts += 1;
                    }
                    if ui.button("Now").clicked() {
                        ts = max_ts;
                    }
                    ui.label(format_ts(self.time_mode, ts));
                });
                self.replay_ts = ts;
            } else {
                ui.label("No data yet for this ticker.");
            }
        }

        ui.separator();

        ui.horizontal(|ui| {
            ui.label("History candles:");
            ui.add(
                egui::Slider::new(&mut self.chart.show_candles, 10..=2000)
                    .logarithmic(true),
            );

            ui.separator();
            ui.label("X zoom:");
            ui.add(
                egui::Slider::new(&mut self.chart.x_zoom, 0.25..=4.0)
                    .logarithmic(true),
            );

            if ui.button("Center X").clicked() {
                self.chart.x_pan_secs = 0.0;
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

        const TF_OPTS: &[(u64, &str)] = &[
            (1, "1s"),
            (5, "5s"),
            (10, "10s"),
            (15, "15s"),
            (30, "30s"),
            (60, "1m"),
            (180, "3m"),
            (300, "5m"),
            (900, "15m"),
            (1800, "30m"),
            (3600, "1h"),
            (14400, "4h"),
            (86400, "1d"),
        ];

        ui.horizontal(|ui| {
            ui.label("Timeframe:");
            egui::ComboBox::from_id_source("tf_combo")
                .selected_text(
                    TF_OPTS
                        .iter()
                        .find(|(tf, _)| *tf == self.chart.tf_secs)
                        .map(|(_, label)| *label)
                        .unwrap_or("custom"),
                )
                .show_ui(ui, |ui| {
                    for (tf, label) in TF_OPTS {
                        if ui
                            .selectable_label(
                                self.chart.tf_secs == *tf,
                                *label,
                            )
                            .clicked()
                        {
                            self.chart.tf_secs = *tf;
                        }
                    }
                });
        });

        ui.separator();

        ui.collapsing("Layout & reload", |ui| {
            ui.horizontal(|ui| {
                ui.label("Reload secs:");
                ui.add(
                    egui::DragValue::new(&mut self.reload_secs)
                        .speed(1.0)
                        .clamp_range(1.0..=60.0),
                );
                if ui.button("Reload now").clicked() {
                    self.reload_current_ticker();
                    self.clamp_ts_to_range();
                }
            });

            ui.separator();
            ui.label("Row configs (height & span):");
            for row in 0..GRID_ROWS {
                ui.horizontal(|ui| {
                    ui.label(format!("Row {row}:"));
                    ui.add(
                        egui::Slider::new(
                            &mut self.row_cfgs[row].height_factor,
                            0.5..=3.0,
                        )
                        .text("H"),
                    );
                    egui::ComboBox::from_id_source(format!(
                        "span_row_{row}"
                    ))
                    .selected_text(match self.row_cfgs[row].span_mode {
                        RowSpanMode::Split3 => "3 cols",
                        RowSpanMode::Left2Right1 => "2+1",
                        RowSpanMode::Left1Right2 => "1+2",
                        RowSpanMode::Full => "full",
                    })
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(
                                self.row_cfgs[row].span_mode
                                    == RowSpanMode::Split3,
                                "3 cols",
                            )
                            .clicked()
                        {
                            self.row_cfgs[row].span_mode =
                                RowSpanMode::Split3;
                        }
                        if ui
                            .selectable_label(
                                self.row_cfgs[row].span_mode
                                    == RowSpanMode::Left2Right1,
                                "2+1",
                            )
                            .clicked()
                        {
                            self.row_cfgs[row].span_mode =
                                RowSpanMode::Left2Right1;
                        }
                        if ui
                            .selectable_label(
                                self.row_cfgs[row].span_mode
                                    == RowSpanMode::Left1Right2,
                                "1+2",
                            )
                            .clicked()
                        {
                            self.row_cfgs[row].span_mode =
                                RowSpanMode::Left1Right2;
                        }
                        if ui
                            .selectable_label(
                                self.row_cfgs[row].span_mode
                                    == RowSpanMode::Full,
                                "full",
                            )
                            .clicked()
                        {
                            self.row_cfgs[row].span_mode =
                                RowSpanMode::Full;
                        }
                    });

                    if matches!(
                        self.row_cfgs[row].span_mode,
                        RowSpanMode::Left2Right1 | RowSpanMode::Left1Right2
                    ) {
                        ui.label("Big ratio:");
                        ui.add(
                            egui::Slider::new(
                                &mut self.row_cfgs[row].big_ratio,
                                0.3..=0.8,
                            )
                            .show_value(false),
                        );
                    }
                });
            }
        });

        ui.separator();

        ui.horizontal(|ui| {
            ui.checkbox(&mut self.show_depth, "Depth");
            ui.checkbox(&mut self.show_ladders, "Ladders");
            ui.checkbox(&mut self.show_trades, "Trades");
            ui.checkbox(&mut self.show_volume, "Volume");
        });

        if let Some(snap) = snap_opt {
            ui.separator();
            ui.label(format!(
                "Last mid: {:.4}   Vol: {:.4}",
                snap.last_mid, snap.last_vol
            ));
        }
    }

    fn ui_trading_panel(&mut self, ui: &mut egui::Ui) {
        ui.group(|ui| {
            ui.heading("Trading");

            ui.label("Requires DYDX_TESTNET_MNEMONIC and testnet.toml");

            ui.separator();

            ui.horizontal(|ui| {
                ui.label("Side:");
                if ui
                    .selectable_label(
                        matches!(self.trade_side, OrderSide::Buy),
                        "BUY",
                    )
                    .clicked()
                {
                    self.trade_side = OrderSide::Buy;
                }
                if ui
                    .selectable_label(
                        matches!(self.trade_side, OrderSide::Sell),
                        "SELL",
                    )
                    .clicked()
                {
                    self.trade_side = OrderSide::Sell;
                }
            });

            ui.horizontal(|ui| {
                ui.label("Kind:");
                if ui
                    .selectable_label(
                        matches!(self.trade_kind, TradeKind::Market),
                        "Market",
                    )
                    .clicked()
                {
                    self.trade_kind = TradeKind::Market;
                }
                if ui
                    .selectable_label(
                        matches!(self.trade_kind, TradeKind::Limit),
                        "Limit-ish",
                    )
                    .clicked()
                {
                    self.trade_kind = TradeKind::Limit;
                }
            });

            ui.horizontal(|ui| {
                ui.label("Size (units):");
                ui.add(
                    egui::DragValue::new(&mut self.trade_size_units)
                        .speed(0.001)
                        .clamp_range(0.0..=1000.0),
                );
            });

            ui.horizontal(|ui| {
                ui.label("Leverage (ui only):");
                ui.add(
                    egui::DragValue::new(&mut self.trade_leverage)
                        .speed(0.5)
                        .clamp_range(1.0..=50.0),
                );
            });

            if matches!(self.trade_kind, TradeKind::Limit) {
                ui.horizontal(|ui| {
                    ui.label("Limit price (guard):");
                    ui.add(
                        egui::DragValue::new(&mut self.trade_limit_price)
                            .speed(0.5),
                    );
                });
            }

            if !self.bot_signal.is_empty()
                && self.bot_signal != "none"
            {
                ui.separator();
                ui.label(format!(
                    "Bot: {} size {:.4}  ({})",
                    self.bot_signal, self.bot_size, self.bot_comment
                ));
                if ui.button("Use bot size").clicked() {
                    if self.bot_size > 0.0 {
                        self.trade_size_units = self.bot_size;
                    }
                }
            }

            ui.separator();

            let ticker = self.current_ticker.clone();
            let size_val = self.trade_size_units.max(0.0);
            let size_str = format!("{:.8}", size_val);

            if ui.button("Send order").clicked() {
                if let Ok(size_bd) = BigDecimal::from_str(&size_str) {
                    let cmd = TradeCmd {
                        ticker: ticker.clone(),
                        side: self.trade_side.clone(),
                        size: size_bd,
                        kind: self.trade_kind.clone(),
                        limit_price: self.trade_limit_price,
                        leverage: self.trade_leverage,
                    };
                    let _ = self.trade_tx.try_send(cmd);
                    self.last_order_msg = format!(
                        "Sent {:?} {:?} {} size {}",
                        self.trade_kind,
                        self.trade_side,
                        ticker,
                        size_str
                    );
                    append_trade_csv(
                        &ticker,
                        "gui_manual",
                        &format!("{:?}", self.trade_side),
                        &size_str,
                    );
                } else {
                    self.last_order_msg =
                        "Invalid size for order".to_string();
                }
            }

            ui.checkbox(
                &mut self.bot_auto_trade,
                "Bot auto-trade (fire when script signals)",
            );

            if !self.last_order_msg.is_empty() {
                ui.separator();
                ui.label(&self.last_order_msg);
            }
        });
    }

    fn ui_script_engine(&mut self, ui: &mut egui::Ui) {
        ui.group(|ui| {
            ui.heading("Script engine (Rhai)");

            ui.horizontal(|ui| {
                if ui.button("Run now").clicked() {
                    self.script_last_run_ts = 0;
                }
                ui.checkbox(
                    &mut self.script_auto_run,
                    "Auto run each refresh",
                );
            });

            ui.separator();

            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut self.script_text)
                            .desired_rows(18)
                            .desired_width(f32::INFINITY),
                    );
                });

            if let Some(err) = &self.script_last_error {
                ui.separator();
                ui.colored_label(Color32::RED, err);
            } else {
                ui.separator();
                ui.colored_label(
                    Color32::LIGHT_GREEN,
                    format!(
                        "Last signal: {} size {:.4}  {}",
                        self.bot_signal, self.bot_size, self.bot_comment
                    ),
                );
            }
        });
    }

    fn ui_recent_trades(&self, ui: &mut egui::Ui, snap: &Snapshot) {
        ui.group(|ui| {
            ui.heading("Recent trades");
            egui::ScrollArea::vertical().show(ui, |ui| {
                egui::Grid::new("recent_trades_grid")
                    .striped(true)
                    .show(ui, |ui| {
                        ui.label("Time");
                        ui.label("Side");
                        ui.label("Size");
                        ui.end_row();

                        for tr in snap.trades.iter().rev() {
                            ui.label(format_ts(self.time_mode, tr.ts));
                            ui.label(&tr.side);
                            ui.label(&tr.size_str);
                            ui.end_row();
                        }
                    });
            });
        });
    }

    fn ui_ladders(&self, ui: &mut egui::Ui, snap: &Snapshot) {
        ui.group(|ui| {
            ui.heading("Ladders (top 20)");

            ui.columns(2, |cols| {
                cols[0].label("Bids");
                egui::Grid::new("bids_grid")
                    .striped(true)
                    .show(&mut cols[0], |ui| {
                        ui.label("Price");
                        ui.label("Size");
                        ui.end_row();
                        for (k, s) in snap.bids.iter().rev().take(20) {
                            let p = key_to_price(*k);
                            ui.label(format!("{:>9.2}", p));
                            ui.label(format!("{:>8.4}", s));
                            ui.end_row();
                        }
                    });

                cols[1].label("Asks");
                egui::Grid::new("asks_grid")
                    .striped(true)
                    .show(&mut cols[1], |ui| {
                        ui.label("Price");
                        ui.label("Size");
                        ui.end_row();
                        for (k, s) in snap.asks.iter().take(20) {
                            let p = key_to_price(*k);
                            ui.label(format!("{:>9.2}", p));
                            ui.label(format!("{:>8.4}", s));
                            ui.end_row();
                        }
                    });
            });
        });
    }

    fn ui_depth_plot(&self, ui: &mut egui::Ui, snap: &Snapshot, height: f32) {
        let mut bid_points = Vec::new();
        let mut ask_points = Vec::new();

        let mut cum = 0.0;
        for (k, s) in snap.bids.iter().rev() {
            let p = key_to_price(*k);
            cum += *s;
            bid_points.push((p, cum));
        }

        cum = 0.0;
        for (k, s) in snap.asks.iter() {
            let p = key_to_price(*k);
            cum += *s;
            ask_points.push((p, cum));
        }

        Plot::new("depth_plot")
            .height(height)
            .show(ui, |plot_ui| {
                if !bid_points.is_empty() {
                    let pts: PlotPoints = bid_points
                        .iter()
                        .map(|(x, y)| [*x, *y])
                        .collect::<Vec<_>>()
                        .into();
                    plot_ui.line(
                        Line::new(pts)
                            .color(Color32::from_rgb(80, 200, 120))
                            .name("Bids"),
                    );
                }
                if !ask_points.is_empty() {
                    let pts: PlotPoints = ask_points
                        .iter()
                        .map(|(x, y)| [*x, *y])
                        .collect::<Vec<_>>()
                        .into();
                    plot_ui.line(
                        Line::new(pts)
                            .color(Color32::from_rgb(220, 80, 80))
                            .name("Asks"),
                    );
                }
            });
    }

    fn ui_candles_and_volume(
        &mut self,
        ui: &mut egui::Ui,
        snap: &Snapshot,
        height: f32,
    ) {
        if snap.candles.is_empty() {
            ui.label("No candles yet at this TF.");
            return;
        }

        let series = &snap.candles;
        let len = series.len();
        let window_len = self.chart.show_candles.min(len).max(1);
        let visible = &series[len - window_len..];

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

        let candles_h = height * 0.7;
        let volume_h = if self.show_volume { height * 0.3 } else { 0.0 };

        let tf = self.chart.tf_secs as f64;
        let last = visible.last().unwrap();
        let x_center = last.t as f64 + tf * 0.5;
        let base_span = tf * self.chart.show_candles as f64;
        let span = base_span / self.chart.x_zoom.max(1e-6);
        let x_min = x_center - span * 0.5 + self.chart.x_pan_secs;
        let x_max = x_center + span * 0.5 + self.chart.x_pan_secs;

        ui.allocate_ui(
            egui::vec2(ui.available_width(), candles_h),
            |ui| {
                let mode = self.time_mode;

                let plot_resp = Plot::new("candles_plot")
                    .height(candles_h)
                    .include_y(y_min)
                    .include_y(y_max)
                    .allow_drag(true)
                    .allow_zoom(true)
                    .x_axis_formatter(move |mark, _range, _| {
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

                            let color = if c.close >= c.open {
                                Color32::from_rgb(40, 200, 120)
                            } else {
                                Color32::from_rgb(220, 60, 60)
                            };

                            let wick_pts: PlotPoints =
                                vec![[mid, c.low], [mid, c.high]].into();
                            plot_ui.line(
                                Line::new(wick_pts)
                                    .color(color)
                                    .width(1.0),
                            );

                            let body_pts: PlotPoints = vec![
                                [left, bot],
                                [left, top],
                                [right, top],
                                [right, bot],
                                [left, bot],
                            ]
                            .into();
                            plot_ui.line(
                                Line::new(body_pts)
                                    .color(color)
                                    .width(2.0),
                            );
                        }
                    });

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
                    let center =
                        (self.chart.y_min + self.chart.y_max) * 0.5;
                    let half_span = (self.chart.y_max - self.chart.y_min)
                        .max(1e-6)
                        * factor
                        * 0.5;
                    self.chart.y_min = center - half_span;
                    self.chart.y_max = center + half_span;
                }
            },
        );

        if self.show_volume {
            ui.separator();
            ui.allocate_ui(
                egui::vec2(ui.available_width(), volume_h),
                |ui| {
                    let mode = self.time_mode;

                    let plot_resp = Plot::new("volume_plot")
                        .height(volume_h)
                        .include_y(0.0)
                        .allow_drag(true)
                        .allow_zoom(true)
                        .x_axis_formatter(move |mark, _range, _| {
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

                            plot_ui.set_plot_bounds(
                                PlotBounds::from_min_max(
                                    [x_min, 0.0],
                                    [x_max, y_max_v],
                                ),
                            );

                            for c in visible {
                                let left = c.t as f64;
                                let mid = left + tf * 0.5;
                                let color =
                                    Color32::from_rgb(120, 170, 240);

                                let pts: PlotPoints =
                                    vec![[mid, 0.0], [mid, c.volume]]
                                        .into();
                                plot_ui.line(
                                    Line::new(pts)
                                        .color(color)
                                        .width(2.0),
                                );
                            }
                        });

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
                        let center = (self.chart.y_min
                            + self.chart.y_max)
                            * 0.5;
                        let half_span = (self.chart.y_max
                            - self.chart.y_min)
                            .max(1e-6)
                            * factor
                            * 0.5;
                        self.chart.y_min = center - half_span;
                        self.chart.y_max = center + half_span;
                    }
                },
            );
        }
    }

    fn ui_grid(&mut self, ui: &mut egui::Ui, snap_opt: Option<&Snapshot>) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for row in 0..GRID_ROWS {
                    let cfg = self.row_cfgs[row];
                    let row_height = 140.0 * cfg.height_factor;
                    let width = ui.available_width();

                    ui.allocate_ui(
                        egui::vec2(width, row_height),
                        |row_ui| {
                            row_ui.horizontal(|row_ui| {
                                let total_w = row_ui.available_width();
                                let (w0, w1, w2) = match cfg.span_mode {
                                    RowSpanMode::Split3 => {
                                        let w = total_w / 3.0;
                                        (w, w, w)
                                    }
                                    RowSpanMode::Left2Right1 => {
                                        let big = total_w * cfg.big_ratio;
                                        let small = total_w - big;
                                        (big, small, 0.0)
                                    }
                                    RowSpanMode::Left1Right2 => {
                                        let big = total_w * cfg.big_ratio;
                                        let small = total_w - big;
                                        (small, big, 0.0)
                                    }
                                    RowSpanMode::Full => {
                                        (total_w, 0.0, 0.0)
                                    }
                                };

                                let mut next_cell = |idx: usize,
                                                     w: f32,
                                                     f: &mut dyn FnMut(
                                                         &mut egui::Ui,
                                                     )| {
                                    if w <= 0.0 {
                                        return;
                                    }
                                    row_ui
                                        .allocate_ui(
                                            egui::vec2(w, row_height),
                                            |cell_ui| {
                                                cell_ui.set_clip_rect(
                                                    cell_ui
                                                        .clip_rect()
                                                        .intersect(
                                                            cell_ui
                                                                .max_rect(),
                                                        ),
                                                );
                                                f(cell_ui);
                                            },
                                        )
                                        .response
                                        .on_hover_text(format!(
                                            "row{}_col{}",
                                            row, idx
                                        ));
                                };

                                match row {
                                    0 => {
                                        if let Some(snap) = snap_opt {
                                            next_cell(
                                                0,
                                                w0,
                                                &mut |cell| {
                                                    self.ui_candles_and_volume(
                                                        cell,
                                                        snap,
                                                        row_height,
                                                    );
                                                },
                                            );
                                        } else {
                                            next_cell(
                                                0,
                                                w0,
                                                &mut |cell| {
                                                    cell.label(
                                                        "No snapshot yet.",
                                                    );
                                                },
                                            );
                                        }
                                    }
                                    1 => {
                                        if let Some(snap) = snap_opt {
                                            next_cell(
                                                0,
                                                w0,
                                                &mut |cell| {
                                                    if self.show_depth {
                                                        self.ui_depth_plot(
                                                            cell,
                                                            snap,
                                                            row_height,
                                                        );
                                                    } else {
                                                        cell.label(
                                                            "Depth hidden",
                                                        );
                                                    }
                                                },
                                            );
                                            next_cell(
                                                1,
                                                w1,
                                                &mut |cell| {
                                                    if self.show_ladders {
                                                        self.ui_ladders(
                                                            cell, snap,
                                                        );
                                                    } else {
                                                        cell.label(
                                                            "Ladders hidden",
                                                        );
                                                    }
                                                },
                                            );
                                        } else {
                                            next_cell(
                                                0,
                                                w0,
                                                &mut |cell| {
                                                    cell.label(
                                                        "No depth/ladders yet.",
                                                    );
                                                },
                                            );
                                        }
                                        next_cell(
                                            2,
                                            w2.max(w1),
                                            &mut |cell| {
                                                self.ui_trading_panel(
                                                    cell,
                                                );
                                            },
                                        );
                                    }
                                    2 => {
                                        next_cell(
                                            0,
                                            w0,
                                            &mut |cell| {
                                                self.ui_script_engine(
                                                    cell,
                                                );
                                            },
                                        );
                                        next_cell(
                                            1,
                                            w1,
                                            &mut |cell| {
                                                cell.group(|ui| {
                                                    ui.heading(
                                                        "Bot status",
                                                    );
                                                    ui.label(
                                                        format!(
"Signal: {}  size {:.4}",
self.bot_signal,
self.bot_size
                                                        ),
                                                    );
                                                    ui.label(
                                                        format!(
                                                            "Comment: {}",
                                                            self
                                                                .bot_comment
                                                        ),
                                                    );
                                                    ui.label(
                                                        format!(
"Auto-trade: {}",
if self.bot_auto_trade {
"ON"
} else {
"OFF"
}
                                                        ),
                                                    );
                                                });
                                            },
                                        );
                                        if let Some(snap) = snap_opt {
                                            next_cell(
                                                2,
                                                w2,
                                                &mut |cell| {
                                                    if self.show_trades {
                                                        self.ui_recent_trades(
                                                            cell, snap,
                                                        );
                                                    } else {
                                                        cell.label(
                                                            "Trades hidden",
                                                        );
                                                    }
                                                },
                                            );
                                        }
                                    }
                                    _ => {
                                        next_cell(
                                            0,
                                            w0,
                                            &mut |cell| {
                                                cell.label(
                                                    format!(
                                                        "Row {row} (free)",
                                                    ),
                                                );
                                            },
                                        );
                                    }
                                }
                            });
                        },
                    );

                    ui.add_space(6.0);
                }
            });
    }
}

impl eframe::App for ComboApp {
    fn update(
        &mut self,
        ctx: &egui::Context,
        _frame: &mut eframe::Frame,
    ) {
        let now = now_unix();
        if now.saturating_sub(self.last_reload_ts) as f64 >= self.reload_secs {
            self.reload_current_ticker();
            self.clamp_ts_to_range();
            self.last_reload_ts = now;
        }

        let snap_opt = self.current_snap();

        if self.script_auto_run {
            if let Some(ref snap) = snap_opt {
                if now.saturating_sub(self.script_last_run_ts) >= 0 {
                    self.run_script(snap);
                }
            }
        }

        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            self.ui_top_bar(ui, snap_opt.as_ref());
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            self.ui_grid(ui, snap_opt.as_ref());
        });

        ctx.request_repaint_after(Duration::from_millis(50));
    }
}

// ---------- async trade executor (real orders) ----------

async fn run_trader(mut rx: mpsc::Receiver<TradeCmd>) {
    let config = match ClientConfig::from_file("client/tests/testnet.toml").await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[trader] failed to load testnet.toml: {e}");
            return;
        }
    };

    let raw = match std::env::var("DYDX_TESTNET_MNEMONIC") {
        Ok(v) => v,
        Err(_) => {
            eprintln!(
                "[trader] DYDX_TESTNET_MNEMONIC not set; trading disabled"
            );
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
        let TradeCmd {
            ticker,
            side,
            size,
            kind,
            limit_price,
            leverage: _,
        } = cmd;

        eprintln!(
            "[trader] {:?} {:?} {} size {} (limit guard: {})",
            kind, side, ticker, size, limit_price
        );

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

        let mut builder = OrderBuilder::new(market, sub.clone())
            .market(side.clone(), size.clone())
            .reduce_only(false)
            .time_in_force(TimeInForce::Unspecified)
            .until(h.ahead(10));

        if limit_price > 0.0 {
            // placeholder "price guard" wiring; you can refine the
            // Price type for real limit orders later.
            builder = builder.price(100);
        }

        let (_id, order) = match builder.build(123456) {
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
                    "trader",
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

// ---------- main ----------

fn main() {
    init_crypto_provider();

    let base_dir = PathBuf::from("data");
    let tickers = vec![
        "ETH-USD".to_string(),
        "BTC-USD".to_string(),
        "SOL-USD".to_string(),
    ];

    let mut ticker_data = HashMap::new();
    for tk in &tickers {
        if let Some(td) = load_ticker_data(&base_dir, tk) {
            ticker_data.insert(tk.clone(), td);
        }
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let (trade_tx, trade_rx) = mpsc::channel::<TradeCmd>(64);

    rt.spawn(run_trader(trade_rx));

    let native_options = eframe::NativeOptions::default();

    let app = ComboApp::new(base_dir, ticker_data, tickers, trade_tx);

    if let Err(e) = eframe::run_native(
        "dYdX CSV Live + Replay + Script Bot (full_gui_x14)",
        native_options,
        Box::new(|_cc| Box::new(app)),
    ) {
        eprintln!("eframe error: {e}");
    }
}
