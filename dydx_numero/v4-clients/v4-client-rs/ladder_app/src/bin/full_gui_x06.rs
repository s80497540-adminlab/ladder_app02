// ladder_app/src/bin/full_gui_x06.rs
//
// CSV-driven dYdX GUI (daemon as engine) + Rhai script/bot
//
// - Reads CSVs written by your daemon under ./data:
//     data/orderbook_{TICKER}.csv
//     data/trades_{TICKER}.csv
//
//   orderbook CSV format (one per line):
//     ts,ticker,kind,side,price,size
//       kind: "book_init" | "delta" | ...
//       side: "bid" | "ask"
//
//   trades CSV format (one per line):
//     ts,ticker,source,side,size_str
//
// - Tickers: ETH-USD, BTC-USD, SOL-USD
//
// Modes:
//   Live   = show snapshot at latest timestamp from CSV for current ticker
//   Replay = slider over full history range for current ticker
//
// UI:
//   - Depth chart
//   - Ladders (top bids / asks)
//   - Candles (mid-price) + Volume
//   - Time: Unix vs Local
//   - Y-axis auto/manual + Shift+wheel vertical zoom
//   - Themes: classic / dark / neon / high_contrast / pastel
//
// Script engine (Rhai):
//   You can write code that sees market + UI state and:
//     - Tweaks UI (tf, history, auto_y, y_min, y_max, show_* flags, reload frequency, theme)
//     - Emits trade signals (buy/sell, market/limit, size, price, leverage)
//
//   Inputs you can READ in the script:
//     tf              : i64    (seconds; 1..86400)
//     history         : i64    (# candles)
//     auto_y          : bool
//     y_min, y_max    : f64
//     show_depth      : bool
//     show_ladders    : bool
//     show_trades     : bool
//     show_volume     : bool
//     reload_secs     : f64
//     theme           : string
//     last_price      : f64
//     last_open       : f64
//     last_high       : f64
//     last_low        : f64
//     last_close      : f64
//     last_volume     : f64
//     bid, ask, mid   : f64
//     spread          : f64
//     mode            : string ("live" / "replay")
//     ticker          : string
//     now_ts          : i64
//
//   Outputs you can WRITE:
//     tf, history, auto_y, y_min, y_max, show_* , reload_secs, theme
//     signal          : "none" | "buy" | "sell" | "buy_limit" | "sell_limit"
//     signal_kind     : "market" | "limit"
//     signal_size     : f64
//     signal_price    : f64
//     signal_leverage : f64
//
//   The Rust side reads these and logs them as “bot actions”.
//   (You can later wire TradeCmd => real dYdX orders.)
//
// ---------------------------------------------------------------------

mod candle_agg;

use candle_agg::{Candle, CandleAgg};

use chrono::{Local, TimeZone};
use eframe::egui;
use egui::{Color32, RichText};
use egui_plot::{Line, Plot, PlotBounds, PlotPoints, VLine};
use rhai::{Engine, Scope};

use std::cmp::{max, min};
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

// ---------------- basic types & helpers ----------------

type PriceKey = i64;

fn price_to_key(price: f64) -> PriceKey {
    (price * 10_000.0).round() as PriceKey
}

fn key_to_price(key: PriceKey) -> f64 {
    key as f64 / 10_000.0
}

fn format_unix_local(ts: u64) -> String {
    let dt = Local
        .timestamp_opt(ts as i64, 0)
        .single()
        .unwrap_or_else(|| Local.timestamp_opt(0, 0).single().unwrap());
    dt.format("%Y-%m-%d %H:%M:%S").to_string()
}

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
        TimeDisplayMode::Local => format_unix_local(ts),
    }
}

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
            show_candles: 300,
            auto_y: true,
            y_min: 0.0,
            y_max: 0.0,
            x_zoom: 1.0,
            x_pan_secs: 0.0,
            selected_tf: 60, // 1m default
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Live,
    Replay,
}

// ---------------- CSV structs ----------------

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
    candles_30s: Vec<Candle>,
    candles_1m: Vec<Candle>,
    candles_3m: Vec<Candle>,
    candles_5m: Vec<Candle>,
    last_mid: f64,
    last_vol: f64,
    trades: Vec<TradeCsvEvent>,
}

// -------------- CSV loaders (daemon-compatible) --------------

fn load_book_csv(path: &Path, ticker: &str) -> Vec<BookCsvEvent> {
    if !path.exists() {
        return Vec::new();
    }
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let reader = BufReader::new(file);
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
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let reader = BufReader::new(file);
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

// reconstruct full snapshot at target_ts from events
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
        candles_30s: tf_30s.series().to_vec(),
        candles_1m: tf_1m.series().to_vec(),
        candles_3m: tf_3m.series().to_vec(),
        candles_5m: tf_5m.series().to_vec(),
        last_mid,
        last_vol,
        trades,
    }
}

// -------------- TradeCmd + stub trader --------------

#[derive(Debug)]
enum TradeCmd {
    MarketOrder {
        ticker: String,
        side: String,
        size: f64,
        leverage: f64,
        source: String, // "script" | "manual"
    },
    LimitOrder {
        ticker: String,
        side: String,
        size: f64,
        price: f64,
        leverage: f64,
        source: String,
    },
}

// For now this just logs to stderr; you can later wire it to real dYdX Node.
async fn run_trader(mut rx: mpsc::Receiver<TradeCmd>) {
    while let Some(cmd) = rx.recv().await {
        eprintln!("[trader] {:?}", cmd);
    }
}

// ---------------- Theme handling ----------------

#[derive(Clone)]
enum UiTheme {
    Classic,
    Dark,
    Neon,
    HighContrast,
    Pastel,
}

impl UiTheme {
    fn from_name(name: &str) -> Self {
        match name {
            "dark" => UiTheme::Dark,
            "neon" => UiTheme::Neon,
            "high_contrast" => UiTheme::HighContrast,
            "pastel" => UiTheme::Pastel,
            _ => UiTheme::Classic,
        }
    }

    fn name(&self) -> &'static str {
        match self {
            UiTheme::Classic => "classic",
            UiTheme::Dark => "dark",
            UiTheme::Neon => "neon",
            UiTheme::HighContrast => "high_contrast",
            UiTheme::Pastel => "pastel",
        }
    }
}

// ---------------- Main app ----------------

struct ComboApp {
    // mode + time
    mode: Mode,
    time_mode: TimeDisplayMode,

    // tickers
    tickers: Vec<String>,
    current_ticker: String,

    // chart + layout flags
    chart: ChartSettings,
    show_depth: bool,
    show_ladders: bool,
    show_trades: bool,
    show_volume: bool,

    // CSV data
    base_dir: String,
    data: HashMap<String, TickerData>,
    reload_secs: f32,
    last_reload: Instant,

    // live / replay timestamps and snapshots
    live_ts: u64,
    replay_ts: u64,
    live_snapshot: Option<Snapshot>,
    replay_snapshot: Option<Snapshot>,

    // trading
    trade_tx: mpsc::Sender<TradeCmd>,
    trade_size_input: f64,
    trade_leverage_input: f64,
    trade_price_input: f64,
    trade_is_limit: bool,
    last_order_msg: String,

    // theme
    theme: UiTheme,
    theme_name: String,

    // script engine
    engine: Engine,
    scope: Scope<'static>,
    script_text: String,
    script_error: Option<String>,
    script_auto_run: bool,
    script_last_run: Option<Instant>,
    last_bot_info: String,
}

impl ComboApp {
    fn new(
        base_dir: String,
        initial_data: HashMap<String, TickerData>,
        tickers: Vec<String>,
        trade_tx: mpsc::Sender<TradeCmd>,
    ) -> Self {
        let theme = UiTheme::Classic;
        let theme_name = theme.name().to_string();

        let current_ticker = if tickers.contains(&"ETH-USD".to_string()) {
            "ETH-USD".to_string()
        } else {
            tickers.first().cloned().unwrap_or_else(|| "ETH-USD".to_string())
        };

        let mut live_ts = 0u64;
        let mut replay_ts = 0u64;

        if let Some(td) = initial_data.get(&current_ticker) {
            live_ts = td.max_ts;
            replay_ts = td.max_ts;
        }

        // script engine + scope
        let mut engine = Engine::new();
        engine.set_max_operations(20_000);

        let mut scope: Scope<'static> = Scope::new();

        // chart/UI knobs
        scope.set_value("tf", 60_i64);
        scope.set_value("history", 300_i64);
        scope.set_value("auto_y", true);
        scope.set_value("y_min", 0.0_f64);
        scope.set_value("y_max", 0.0_f64);
        scope.set_value("show_depth", true);
        scope.set_value("show_ladders", true);
        scope.set_value("show_trades", true);
        scope.set_value("show_volume", true);
        scope.set_value("reload_secs", 1.0_f64);
        scope.set_value("theme", theme_name.clone());

        // market inputs
        scope.set_value("last_price", 0.0_f64);
        scope.set_value("last_open", 0.0_f64);
        scope.set_value("last_high", 0.0_f64);
        scope.set_value("last_low", 0.0_f64);
        scope.set_value("last_close", 0.0_f64);
        scope.set_value("last_volume", 0.0_f64);
        scope.set_value("bid", 0.0_f64);
        scope.set_value("ask", 0.0_f64);
        scope.set_value("mid", 0.0_f64);
        scope.set_value("spread", 0.0_f64);
        scope.set_value("mode", "live".to_string());
        scope.set_value("ticker", current_ticker.clone());
        scope.set_value("now_ts", 0_i64);

        // bot outputs
        scope.set_value("signal", "none".to_string());
        scope.set_value("signal_kind", "market".to_string());
        scope.set_value("signal_size", 0.0_f64);
        scope.set_value("signal_price", 0.0_f64);
        scope.set_value("signal_leverage", 1.0_f64);

        let default_script = r#"
// === Rhai script ===
// See Rust header for full list of inputs/outputs.

// Example: simple ETH-USD breakout bot + UI tweaks

signal = "none";

if mode == "live" && ticker == "ETH-USD" {

    // UI preferences
    tf        = 60;      // 1m candles
    history   = 300;
    auto_y    = true;
    theme     = "dark";
    show_depth   = true;
    show_ladders = true;
    show_trades  = true;
    show_volume  = true;

    let breakout_up   = 4500.0;
    let breakout_down = 4400.0;
    let sz            = 0.01;
    let lev           = 3.0;

    if last_price > breakout_up && spread < 5.0 {
        signal          = "buy";
        signal_kind     = "market";
        signal_size     = sz;
        signal_leverage = lev;
    } else if last_price < breakout_down && spread < 5.0 {
        signal          = "sell";
        signal_kind     = "market";
        signal_size     = sz;
        signal_leverage = lev;
    }
}
"#.to_string();

        Self {
            mode: Mode::Live,
            time_mode: TimeDisplayMode::Local,
            tickers,
            current_ticker,
            chart: ChartSettings::default(),
            show_depth: true,
            show_ladders: true,
            show_trades: true,
            show_volume: true,
            base_dir,
            data: initial_data,
            reload_secs: 1.0,
            last_reload: Instant::now(),
            live_ts,
            replay_ts,
            live_snapshot: None,
            replay_snapshot: None,
            trade_tx,
            trade_size_input: 0.01,
            trade_leverage_input: 1.0,
            trade_price_input: 0.0,
            trade_is_limit: false,
            last_order_msg: String::new(),
            theme,
            theme_name,
            engine,
            scope,
            script_text: default_script,
            script_error: None,
            script_auto_run: false,
            script_last_run: None,
            last_bot_info: String::new(),
        }
    }

    fn current_ticker_data(&self) -> Option<&TickerData> {
        self.data.get(&self.current_ticker)
    }

    fn reload_all_data_if_needed(&mut self) {
        let now = Instant::now();
        if now
            .duration_since(self.last_reload)
            .as_secs_f32()
            < self.reload_secs
        {
            return;
        }
        self.last_reload = now;

        for tk in &self.tickers {
            if let Some(td) = load_ticker_data(&self.base_dir, tk) {
                self.data.insert(tk.clone(), td);
            }
        }

        self.ensure_ts_in_range();
        self.update_snapshots();
    }

    fn ensure_ts_in_range(&mut self) {
        let (min_ts, max_ts) = match self.data.get(&self.current_ticker) {
            Some(td) => (td.min_ts, td.max_ts),
            None => return,
        };

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

    fn update_snapshots(&mut self) {
        let (live_ts, replay_ts) = (self.live_ts, self.replay_ts);
        let (live_snap, replay_snap) = if let Some(td) = self.data.get(&self.current_ticker) {
            (
                Some(compute_snapshot_for(td, live_ts)),
                Some(compute_snapshot_for(td, replay_ts)),
            )
        } else {
            (None, None)
        };

        self.live_snapshot = live_snap;
        self.replay_snapshot = replay_snap;
    }

    fn set_theme_from_name(&mut self, name: &str) {
        self.theme = UiTheme::from_name(name);
        self.theme_name = self.theme.name().to_string();
    }

    // --------------- script engine: sync -> run -> apply ---------------

    fn sync_scope_from_state(&mut self, snap: Option<&Snapshot>) {
        // chart / UI knobs
        self.scope
            .set_value("tf", self.chart.selected_tf as i64);
        self.scope
            .set_value("history", self.chart.show_candles as i64);
        self.scope.set_value("auto_y", self.chart.auto_y);
        self.scope.set_value("y_min", self.chart.y_min);
        self.scope.set_value("y_max", self.chart.y_max);
        self.scope.set_value("show_depth", self.show_depth);
        self.scope.set_value("show_ladders", self.show_ladders);
        self.scope.set_value("show_trades", self.show_trades);
        self.scope.set_value("show_volume", self.show_volume);
        self.scope
            .set_value("reload_secs", self.reload_secs as f64);
        self.scope
            .set_value("theme", self.theme_name.clone());

        // mode / ticker / time
        let mode_str = match self.mode {
            Mode::Live => "live",
            Mode::Replay => "replay",
        };
        self.scope
            .set_value("mode", mode_str.to_string());
        self.scope
            .set_value("ticker", self.current_ticker.clone());

        let now_ts = match self.mode {
            Mode::Live => self.live_ts as i64,
            Mode::Replay => self.replay_ts as i64,
        };
        self.scope.set_value("now_ts", now_ts);

        // market values
        let mut last_price = 0.0_f64;
        let mut last_open = 0.0_f64;
        let mut last_high = 0.0_f64;
        let mut last_low = 0.0_f64;
        let mut last_close = 0.0_f64;
        let mut last_volume = 0.0_f64;
        let mut bid = 0.0_f64;
        let mut ask = 0.0_f64;
        let mut mid = 0.0_f64;

        if let Some(snap) = snap {
            let series_vec = self.series_for_snap(snap);
            if let Some(c) = series_vec.last() {
                last_price = c.close;
                last_open = c.open;
                last_high = c.high;
                last_low = c.low;
                last_close = c.close;
                last_volume = c.volume;
            }
            if let Some((k, s)) = snap.bids.iter().next_back() {
                bid = key_to_price(*k);
                let _bid_size = s;
            }
            if let Some((k, s)) = snap.asks.iter().next() {
                ask = key_to_price(*k);
                let _ask_size = s;
            }
            if bid > 0.0 && ask > 0.0 {
                mid = 0.5 * (bid + ask);
            } else if snap.last_mid > 0.0 {
                mid = snap.last_mid;
            }
        }

        let spread = if bid > 0.0 && ask > 0.0 {
            ask - bid
        } else {
            0.0
        };

        self.scope.set_value("last_price", last_price);
        self.scope.set_value("last_open", last_open);
        self.scope.set_value("last_high", last_high);
        self.scope.set_value("last_low", last_low);
        self.scope.set_value("last_close", last_close);
        self.scope.set_value("last_volume", last_volume);
        self.scope.set_value("bid", bid);
        self.scope.set_value("ask", ask);
        self.scope.set_value("mid", mid);
        self.scope.set_value("spread", spread);
    }

    fn apply_scope_to_state(&mut self) {
        if let Some(tf) = self.scope.get_value::<i64>("tf") {
            let tf = tf.clamp(1, 86_400) as u64;
            self.chart.selected_tf = tf;
        }
        if let Some(h) = self.scope.get_value::<i64>("history") {
            self.chart.show_candles = h.clamp(10, 2_000) as usize;
        }
        if let Some(auto_y) = self.scope.get_value::<bool>("auto_y") {
            self.chart.auto_y = auto_y;
        }
        if let Some(y_min) = self.scope.get_value::<f64>("y_min") {
            self.chart.y_min = y_min;
        }
        if let Some(y_max) = self.scope.get_value::<f64>("y_max") {
            self.chart.y_max = y_max;
        }
        if let Some(v) = self.scope.get_value::<bool>("show_depth") {
            self.show_depth = v;
        }
        if let Some(v) = self.scope.get_value::<bool>("show_ladders") {
            self.show_ladders = v;
        }
        if let Some(v) = self.scope.get_value::<bool>("show_trades") {
            self.show_trades = v;
        }
        if let Some(v) = self.scope.get_value::<bool>("show_volume") {
            self.show_volume = v;
        }
        if let Some(rs) = self.scope.get_value::<f64>("reload_secs") {
            self.reload_secs = rs.clamp(0.1, 60.0) as f32;
        }
        if let Some(theme) = self.scope.get_value::<String>("theme") {
            self.set_theme_from_name(&theme);
        }
    }

    fn handle_script_signal(&mut self) {
        let sig = match self.scope.get_value::<String>("signal") {
            Some(s) => s,
            None => return,
        };

        if sig == "none" || sig.is_empty() {
            return;
        }

        let side = if sig.starts_with("buy") {
            "buy"
        } else if sig.starts_with("sell") {
            "sell"
        } else {
            self.last_bot_info =
                format!("Script: unknown signal '{sig}'");
            self.scope
                .set_value("signal", "none".to_string());
            return;
        };

        let kind = self
            .scope
            .get_value::<String>("signal_kind")
            .unwrap_or_else(|| "market".to_string());

        let size = self
            .scope
            .get_value::<f64>("signal_size")
            .unwrap_or(0.0);
        if size <= 0.0 {
            self.last_bot_info =
                "Script: signal_size <= 0, not sending".to_string();
            self.scope
                .set_value("signal", "none".to_string());
            return;
        }

        let lev = self
            .scope
            .get_value::<f64>("signal_leverage")
            .unwrap_or(1.0)
            .max(0.0);

        let price = self
            .scope
            .get_value::<f64>("signal_price")
            .unwrap_or(0.0);

        let ticker = self.current_ticker.clone();
        let side_str = side.to_string();

        let cmd = if kind == "limit" || sig.contains("limit") {
            if price <= 0.0 {
                self.last_bot_info =
                    "Script: limit signal but price <= 0".to_string();
                self.scope
                    .set_value("signal", "none".to_string());
                return;
            }
            TradeCmd::LimitOrder {
                ticker: ticker.clone(),
                side: side_str.clone(),
                size,
                price,
                leverage: lev,
                source: "script".to_string(),
            }
        } else {
            TradeCmd::MarketOrder {
                ticker: ticker.clone(),
                side: side_str.clone(),
                size,
                leverage: lev,
                source: "script".to_string(),
            }
        };

        if self.trade_tx.try_send(cmd).is_ok() {
            self.last_bot_info = format!(
                "Script sent {side} {ticker} size {:.6} lev {:.1} ({kind})",
                size, lev
            );
        } else {
            self.last_bot_info =
                "Script: failed to send TradeCmd (channel full?)".to_string();
        }

        // reset signal
        self.scope
            .set_value("signal", "none".to_string());
    }

    fn run_script(&mut self, snap: Option<Snapshot>) {
        self.sync_scope_from_state(snap.as_ref());

        self.script_error = None;
        match self
            .engine
            .eval_with_scope::<()>(&mut self.scope, &self.script_text)
        {
            Ok(()) => {
                self.apply_scope_to_state();
                self.handle_script_signal();
                self.script_last_run = Some(Instant::now());
            }
            Err(e) => {
                self.script_error = Some(e.to_string());
            }
        }
    }

    // ---------------- UI: top bar ----------------

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

            // ticker
            let tickers = self.tickers.clone();
            ui.menu_button(
                format!("Ticker: {}", self.current_ticker),
                |ui| {
                    for t in &tickers {
                        let selected = *t == self.current_ticker;
                        if ui.selectable_label(selected, t).clicked() {
                            self.current_ticker = t.clone();
                            if let Some(td) = self.current_ticker_data() {
                                self.live_ts = td.max_ts;
                                self.replay_ts = td.max_ts;
                            }
                            self.update_snapshots();
                            ui.close_menu();
                        }
                    }
                },
            );

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

            // show ranges
            if let Some(td) = self.current_ticker_data() {
                ui.separator();
                ui.label(format!(
                    "Range: {} → {}",
                    format_ts(self.time_mode, td.min_ts),
                    format_ts(self.time_mode, td.max_ts)
                ));
            }
        });

        ui.separator();

        // replay-only controls (but we also ensure live_ts tracks max_ts in live mode)
        if let Some((min_ts, max_ts)) = self
            .current_ticker_data()
            .map(|td| (td.min_ts, td.max_ts))
        {
            if matches!(self.mode, Mode::Replay) {
                let mut ts = self.replay_ts.clamp(min_ts, max_ts);
                ui.horizontal(|ui| {
                    ui.label("Replay time:");
                    ui.add(
                        egui::Slider::new(&mut ts, min_ts..=max_ts)
                            .show_value(false)
                            .text("ts"),
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
                self.update_snapshots();
            } else {
                // Live mode: always lock to latest ts for that ticker
                self.live_ts = max_ts;
                self.update_snapshots();
            }
        } else {
            ui.label("No CSV data for this ticker yet (check daemon).");
        }

        ui.separator();

        // chart / layout controls
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
                    .logarithmic(true),
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
            for (label, tf) in [
                ("30s", 30u64),
                ("1m", 60),
                ("3m", 180),
                ("5m", 300),
            ] {
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

        ui.horizontal(|ui| {
            ui.label("Layout:");
            ui.checkbox(&mut self.show_depth, "Depth");
            ui.checkbox(&mut self.show_ladders, "Ladders");
            ui.checkbox(&mut self.show_trades, "Trades");
            ui.checkbox(&mut self.show_volume, "Volume");
        });

        ui.separator();

        ui.horizontal(|ui| {
            ui.label("Theme:");
            for name in ["classic", "dark", "neon", "high_contrast", "pastel"] {
                if ui
                    .selectable_label(self.theme_name == name, name)
                    .clicked()
                {
                    self.set_theme_from_name(name);
                }
            }
        });

        ui.separator();
    }

    // ---------------- UI: script panel ----------------

    fn ui_script_engine(&mut self, ui: &mut egui::Ui, snap: Option<Snapshot>) {
        ui.collapsing("Script / Bot engine", |ui| {
            ui.horizontal(|ui| {
                if ui.button("Run script now").clicked() {
                    self.run_script(snap.clone());
                }
                ui.checkbox(&mut self.script_auto_run, "Auto run");
            });

            if let Some(err) = &self.script_error {
                ui.colored_label(Color32::RED, format!("Script error: {err}"));
            }

            if !self.last_bot_info.is_empty() {
                ui.colored_label(Color32::LIGHT_GREEN, &self.last_bot_info);
            }

            if let Some(last) = self.script_last_run {
                let ago = last.elapsed().as_secs_f32();
                ui.label(format!("Last run: {:.2}s ago", ago));
            }

            ui.separator();
            ui.label("Rhai script (see header for variables):");
            egui::ScrollArea::vertical()
                .max_height(220.0)
                .show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut self.script_text)
                            .font(egui::TextStyle::Monospace)
                            .desired_rows(10)
                            .desired_width(f32::INFINITY),
                    );
                });
        });
    }

    // ---------------- UI: trading panel ----------------

    fn ui_trading_panel(&mut self, ui: &mut egui::Ui) {
        ui.group(|ui| {
            ui.heading("Bot / Manual trade controls");
            ui.label("NOTE: this build only LOGS trades (no real orders yet).");

            ui.separator();

            ui.horizontal(|ui| {
                ui.label("Size:");
                ui.add(
                    egui::DragValue::new(&mut self.trade_size_input)
                        .speed(0.001)
                        .clamp_range(0.0..=1000.0),
                );
                ui.label(self.current_ticker.split('-').next().unwrap_or("UNIT"));
            });

            ui.horizontal(|ui| {
                ui.label("Leverage:");
                ui.add(
                    egui::DragValue::new(&mut self.trade_leverage_input)
                        .speed(0.5)
                        .clamp_range(0.0..=100.0),
                );
            });

            ui.horizontal(|ui| {
                ui.checkbox(&mut self.trade_is_limit, "Limit order");
                if self.trade_is_limit {
                    ui.label("Limit price:");
                    ui.add(
                        egui::DragValue::new(&mut self.trade_price_input)
                            .speed(1.0),
                    );
                }
            });

            ui.horizontal(|ui| {
                if ui.button("Manual BUY").clicked() {
                    self.send_manual_trade("buy");
                }
                if ui.button("Manual SELL").clicked() {
                    self.send_manual_trade("sell");
                }
            });

            if !self.last_order_msg.is_empty() {
                ui.separator();
                ui.label(&self.last_order_msg);
            }
        });
    }

    fn send_manual_trade(&mut self, side: &str) {
        let size = self.trade_size_input.max(0.0);
        if size <= 0.0 {
            self.last_order_msg = "Size must be > 0".to_string();
            return;
        }
        let lev = self.trade_leverage_input.max(0.0);
        let ticker = self.current_ticker.clone();

        let cmd = if self.trade_is_limit {
            if self.trade_price_input <= 0.0 {
                self.last_order_msg =
                    "Limit price must be > 0".to_string();
                return;
            }
            TradeCmd::LimitOrder {
                ticker: ticker.clone(),
                side: side.to_string(),
                size,
                price: self.trade_price_input,
                leverage: lev,
                source: "manual".to_string(),
            }
        } else {
            TradeCmd::MarketOrder {
                ticker: ticker.clone(),
                side: side.to_string(),
                size,
                leverage: lev,
                source: "manual".to_string(),
            }
        };

        if self.trade_tx.try_send(cmd).is_ok() {
            self.last_order_msg = format!(
                "Manual {side} {ticker} size {:.6} lev {:.1} ({})",
                size,
                lev,
                if self.trade_is_limit {
                    "limit"
                } else {
                    "market"
                }
            );
        } else {
            self.last_order_msg =
                "Failed to send trade (channel full?)".to_string();
        }
    }

    // ---------------- UI: depth / ladders / candles ----------------

    fn ui_depth_plot(
        &self,
        ui: &mut egui::Ui,
        snap: &Snapshot,
        height: f32,
    ) {
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

        Plot::new("depth_plot")
            .height(height)
            .allow_zoom(true)
            .allow_drag(true)
            .show(ui, |plot_ui| {
                if !bid_points.is_empty() {
                    let pts: PlotPoints = bid_points
                        .iter()
                        .map(|(x, y)| [*x, *y])
                        .collect::<Vec<_>>()
                        .into();
                    plot_ui
                        .line(Line::new(pts).name("Bids").color(Color32::LIGHT_GREEN));
                }
                if !ask_points.is_empty() {
                    let pts: PlotPoints = ask_points
                        .iter()
                        .map(|(x, y)| [*x, *y])
                        .collect::<Vec<_>>()
                        .into();
                    plot_ui
                        .line(Line::new(pts).name("Asks").color(Color32::LIGHT_RED));
                }
            });
    }

    fn ui_ladders_and_trades(
        &self,
        ui: &mut egui::Ui,
        snap: &Snapshot,
        height: f32,
    ) {
        ui.allocate_ui(egui::vec2(ui.available_width(), height), |ui| {
            ui.columns(2, |cols| {
                // Bids
                cols[0].label(RichText::new("Bids").strong());
                egui::Grid::new("bids_grid")
                    .striped(true)
                    .show(&mut cols[0], |ui| {
                        ui.label("Price");
                        ui.label("Size");
                        ui.end_row();
                        for (k, s) in snap.bids.iter().rev().take(25) {
                            let p = key_to_price(*k);
                            ui.label(format!("{:>9.2}", p));
                            ui.label(format!("{:>9.4}", s));
                            ui.end_row();
                        }
                    });

                // Asks
                cols[1].label(RichText::new("Asks").strong());
                egui::Grid::new("asks_grid")
                    .striped(true)
                    .show(&mut cols[1], |ui| {
                        ui.label("Price");
                        ui.label("Size");
                        ui.end_row();
                        for (k, s) in snap.asks.iter().take(25) {
                            let p = key_to_price(*k);
                            ui.label(format!("{:>9.2}", p));
                            ui.label(format!("{:>9.4}", s));
                            ui.end_row();
                        }
                    });
            });

            ui.separator();

            ui.label(format!(
                "Last mid: {:.2}   Last vol: {:.4}",
                snap.last_mid, snap.last_vol
            ));

            if self.show_trades {
                ui.separator();
                ui.label("Recent trades:");
                egui::ScrollArea::vertical()
                    .max_height(height * 0.6)
                    .show(ui, |ui| {
                        egui::Grid::new("trades_grid")
                            .striped(true)
                            .show(ui, |ui| {
                                ui.label("Time");
                                ui.label("Side");
                                ui.label("Size");
                                ui.label("Source");
                                ui.end_row();

                                for tr in snap.trades.iter().rev() {
                                    ui.label(format_ts(self.time_mode, tr.ts));
                                    ui.label(&tr.side);
                                    ui.label(&tr.size_str);
                                    ui.label(&tr.source);
                                    ui.end_row();
                                }
                            });
                    });
            }
        });
    }

    fn series_for_snap(&self, snap: &Snapshot) -> Vec<Candle> {
        match self.chart.selected_tf {
            30 => snap.candles_30s.clone(),
            60 => snap.candles_1m.clone(),
            180 => snap.candles_3m.clone(),
            300 => snap.candles_5m.clone(),
            _ => snap.candles_1m.clone(),
        }
    }

    fn ui_candles_and_volume(
        &mut self,
        ui: &mut egui::Ui,
        snap: &Snapshot,
        is_live: bool,
    ) {
        let series_vec = self.series_for_snap(snap);
        if series_vec.is_empty() {
            ui.label(if is_live {
                "No candles yet (waiting for CSV / daemon)..."
            } else {
                "No candles at this replay time."
            });
            return;
        }

        let len = series_vec.len();
        let window_len = self.chart.show_candles.min(len).max(1);
        let visible = &series_vec[len - window_len..];

        // Y range
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
        let candles_h = avail_h * 0.7;
        let volume_h = avail_h * 0.3;

        let tf = self.chart.selected_tf as f64;
        let last = visible.last().unwrap();
        let x_center = last.t as f64 + tf * 0.5;
        let base_span = tf * self.chart.show_candles as f64;
        let span = base_span / self.chart.x_zoom.max(1e-6);
        let x_min = x_center - span * 0.5 + self.chart.x_pan_secs;
        let x_max = x_center + span * 0.5 + self.chart.x_pan_secs;

        // Candles
        ui.allocate_ui(egui::vec2(avail_w, candles_h), |ui| {
            let mode = self.time_mode;
            let now_x = match self.mode {
                Mode::Live => self.live_ts as f64,
                Mode::Replay => self.replay_ts as f64,
            };

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
            .x_axis_formatter(move |mark, _bounds, _tr| {
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
                        Color32::GREEN
                    } else {
                        Color32::RED
                    };

                    // wick
                    let wick_pts: PlotPoints = vec![[mid, c.low], [mid, c.high]].into();
                    plot_ui.line(Line::new(wick_pts).color(color));

                    // filled body
                    let body_pts: PlotPoints = vec![
                        [left, bot],
                        [left, top],
                        [right, top],
                        [right, bot],
                        [left, bot],
                    ]
                    .into();
                    plot_ui.line(Line::new(body_pts).color(color).width(2.0));
                }

                plot_ui
                    .vline(VLine::new(now_x).color(Color32::YELLOW).name("now_ts"));
            });

            // vertical zoom via Shift + scroll
            let hovered = plot_resp.response.hovered();
            let mut scroll_y = 0.0_f32;
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

        ui.separator();

        // Volume
        if self.show_volume {
            ui.allocate_ui(egui::vec2(avail_w, volume_h), |ui| {
                let mode = self.time_mode;
                let plot_resp = Plot::new(if is_live {
                    "volume_live"
                } else {
                    "volume_replay"
                })
                .height(volume_h)
                .include_y(0.0)
                .allow_drag(true)
                .allow_zoom(true)
                .x_axis_formatter(move |mark, _bounds, _tr| {
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
                        let color = Color32::from_rgb(120, 170, 240);

                        let line_pts: PlotPoints =
                            vec![[mid, 0.0], [mid, c.volume]].into();
                        plot_ui.line(Line::new(line_pts).color(color).width(2.0));
                    }
                });

                let _hovered = plot_resp.response.hovered();
            });
        }
    }

    // ---------------- UI: live + replay wrappers ----------------

    fn ui_live(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        ui.heading(format!("LIVE {}", self.current_ticker));

        let avail_w = ui.available_width();
        let avail_h = ui.available_height();

        ui.separator();

        // top row: depth + ladders/trades + trading panel
        egui::CollapsingHeader::new("Orderbook / Trades / Trading")
            .default_open(true)
            .show(ui, |ui| {
                let top_h = avail_h * 0.45;
                ui.allocate_ui(egui::vec2(avail_w, top_h), |ui| {
                    ui.columns(3, |cols| {
                        if self.show_depth {
                            self.ui_depth_plot(&mut cols[0], snap, top_h * 0.9);
                        }
                        if self.show_ladders {
                            self.ui_ladders_and_trades(&mut cols[1], snap, top_h * 0.9);
                        }
                        self.ui_trading_panel(&mut cols[2]);
                    });
                });
            });

        ui.separator();

        // candles + volume
        egui::CollapsingHeader::new("Candles + Volume")
            .default_open(true)
            .show(ui, |ui| {
                self.ui_candles_and_volume(ui, snap, true);
            });
    }

    fn ui_replay(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        ui.heading(format!(
            "REPLAY {} @ {}",
            self.current_ticker,
            format_ts(self.time_mode, self.replay_ts)
        ));

        let avail_w = ui.available_width();
        let avail_h = ui.available_height();

        ui.separator();

        egui::CollapsingHeader::new("Orderbook / Trades")
            .default_open(true)
            .show(ui, |ui| {
                let top_h = avail_h * 0.45;
                ui.allocate_ui(egui::vec2(avail_w, top_h), |ui| {
                    ui.columns(2, |cols| {
                        if self.show_depth {
                            self.ui_depth_plot(&mut cols[0], snap, top_h * 0.9);
                        }
                        if self.show_ladders {
                            self.ui_ladders_and_trades(&mut cols[1], snap, top_h * 0.9);
                        }
                    });
                });
            });

        ui.separator();

        egui::CollapsingHeader::new("Candles + Volume")
            .default_open(true)
            .show(ui, |ui| {
                self.ui_candles_and_volume(ui, snap, false);
            });
    }
}

// --------------- eframe::App impl ---------------

impl eframe::App for ComboApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // reload CSVs from daemon periodically
        self.reload_all_data_if_needed();

        // take snapshots by value to avoid borrow hell
        let live_snap = self.live_snapshot.clone();
        let replay_snap = self.replay_snapshot.clone();

        let current_snap = match self.mode {
            Mode::Live => live_snap.clone(),
            Mode::Replay => replay_snap.clone(),
        };

        // script auto-run
        if self.script_auto_run {
            self.run_script(current_snap.clone());
        }

        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            self.ui_top_bar(ui);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    match self.mode {
                        Mode::Live => {
                            if let Some(ref snap) = live_snap {
                                self.ui_live(ui, snap);
                            } else {
                                ui.label("No live snapshot yet (no CSV or daemon just started).");
                            }
                        }
                        Mode::Replay => {
                            if let Some(ref snap) = replay_snap {
                                self.ui_replay(ui, snap);
                            } else {
                                ui.label("No replay snapshot yet (no CSV?).");
                            }
                        }
                    }

                    ui.separator();
                    self.ui_script_engine(ui, current_snap.clone());
                });
        });

        ctx.request_repaint_after(Duration::from_millis(50));
    }
}

// --------------- main ---------------

fn main() {
    let base_dir = "data".to_string();
    let tickers = vec![
        "ETH-USD".to_string(),
        "BTC-USD".to_string(),
        "SOL-USD".to_string(),
    ];

    let mut initial_data = HashMap::new();
    for tk in &tickers {
        if let Some(td) = load_ticker_data(&base_dir, tk) {
            initial_data.insert(tk.clone(), td);
        }
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let (trade_tx, trade_rx) = mpsc::channel::<TradeCmd>(32);
    rt.spawn(run_trader(trade_rx));

    let native_options = eframe::NativeOptions::default();

    let app = ComboApp::new(
        base_dir.clone(),
        initial_data,
        tickers,
        trade_tx.clone(),
    );

    if let Err(e) = eframe::run_native(
        "dYdX CSV Live+Replay + Script Bot",
        native_options,
        Box::new(|_cc| Box::new(app)),
    ) {
        eprintln!("eframe error: {e}");
    }

    drop(rt);
}
