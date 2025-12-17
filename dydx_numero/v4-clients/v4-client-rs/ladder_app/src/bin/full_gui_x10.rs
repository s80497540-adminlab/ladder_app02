// ladder_app/src/bin/full_gui_x11.rs
//
// GUI that reads daemon-generated CSVs under ./data and
// shows per-ticker orderbook depth + candles + volume.
//
// Features:
// - Tickers: ETH-USD / BTC-USD / SOL-USD
// - Live mode: uses latest timestamp from CSVs
// - Replay mode: slider over historical timestamps
// - Candle engine via candle_agg.rs (30s / 1m / 3m / 5m)
// - Bubble metrics computed from top-of-book
// - Rhai script engine:
//     - Reads ctx: mid, last_vol, best_bid/ask, bid/ask walls
//     - Controls: auto_y, show_depth, show_ladders, show_trades, show_volume, reload_secs
//     - Emits: bot_signal, bot_size, bot_comment
//
// Assumes daemon writes:
//   data/orderbook_{TICKER}.csv: ts,ticker,kind,side,price,size
//   data/trades_{TICKER}.csv:    ts,ticker,source,side,size_str
//
// Build & run:
//   cargo run --release -p ladder_app --bin full_gui_x11

mod candle_agg;

use candle_agg::{Candle, CandleAgg};

use chrono::{Local, TimeZone};

use eframe::egui;
use eframe::egui::Color32;
use egui_plot::{Line, Plot, PlotBounds, PlotPoints, VLine};

use rhai::{Engine, Map as RhaiMap, Scope};

use std::cmp::{max, min};
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

// ---------- basic helpers ----------

fn price_to_key(price: f64) -> i64 {
    (price * 10_000.0).round() as i64
}

fn key_to_price(key: i64) -> f64 {
    key as f64 / 10_000.0
}

fn format_ts_local(ts: u64) -> String {
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
        TimeDisplayMode::Local => format_ts_local(ts),
    }
}

// ---------- CSV structures ----------

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
    bids: BTreeMap<i64, f64>,
    asks: BTreeMap<i64, f64>,
    tf_30s: Vec<Candle>,
    tf_1m: Vec<Candle>,
    tf_3m: Vec<Candle>,
    tf_5m: Vec<Candle>,
    last_mid: f64,
    last_vol: f64,
    trades: Vec<TradeCsvEvent>,
}

// ---------- bubble info ----------

#[derive(Clone, Debug, Default)]
struct BubbleInfo {
    best_bid: f64,
    best_ask: f64,
    bid_wall_price: f64,
    bid_wall_size: f64,
    bid_wall_score: f64,
    ask_wall_price: f64,
    ask_wall_size: f64,
    ask_wall_score: f64,
}

fn compute_bubbles(
    bids: &BTreeMap<i64, f64>,
    asks: &BTreeMap<i64, f64>,
    levels: usize,
) -> BubbleInfo {
    let best_bid = bids
        .iter()
        .next_back()
        .map(|(k, _)| key_to_price(*k))
        .unwrap_or(0.0);
    let best_ask = asks
        .iter()
        .next()
        .map(|(k, _)| key_to_price(*k))
        .unwrap_or(0.0);

    fn find_wall(
        side: &BTreeMap<i64, f64>,
        take_rev: bool,
        levels: usize,
    ) -> (f64, f64, f64) {
        let iter: Box<dyn Iterator<Item = (&i64, &f64)>> = if take_rev {
            Box::new(side.iter().rev().take(levels))
        } else {
            Box::new(side.iter().take(levels))
        };

        let mut sizes = Vec::new();
        let mut lv = Vec::new();

        for (k, s) in iter {
            sizes.push(*s);
            lv.push((key_to_price(*k), *s));
        }

        if sizes.is_empty() {
            return (0.0, 0.0, 0.0);
        }

        let avg = sizes.iter().sum::<f64>() / sizes.len() as f64;
        if avg <= 0.0 {
            return (0.0, 0.0, 0.0);
        }

        let mut best_score = 0.0;
        let mut best_price = 0.0;
        let mut best_size = 0.0;

        for (price, size) in lv {
            let score = size / avg;
            if score > best_score {
                best_score = score;
                best_price = price;
                best_size = size;
            }
        }

        (best_price, best_size, best_score)
    }

    let (bid_wall_price, bid_wall_size, bid_wall_score) = find_wall(bids, true, levels);
    let (ask_wall_price, ask_wall_size, ask_wall_score) = find_wall(asks, false, levels);

    BubbleInfo {
        best_bid,
        best_ask,
        bid_wall_price,
        bid_wall_size,
        bid_wall_score,
        ask_wall_price,
        ask_wall_size,
        ask_wall_score,
    }
}

// ---------- CSV loading ----------

fn load_book_csv(path: &Path, ticker: &str) -> Vec<BookCsvEvent> {
    if !path.exists() {
        return Vec::new();
    }
    let Ok(f) = File::open(path) else { return Vec::new() };
    let reader = BufReader::new(f);
    let mut out = Vec::new();

    for line in reader.lines() {
        let Ok(line) = line else { continue };
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

    out.sort_by_key(|e| e.ts);
    out
}

fn load_trades_csv(path: &Path, ticker: &str) -> Vec<TradeCsvEvent> {
    if !path.exists() {
        return Vec::new();
    }
    let Ok(f) = File::open(path) else { return Vec::new() };
    let reader = BufReader::new(f);
    let mut out = Vec::new();

    for line in reader.lines() {
        let Ok(line) = line else { continue };
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

    out.sort_by_key(|e| e.ts);
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

// reconstruct snapshot at target_ts
fn compute_snapshot_for(data: &TickerData, target_ts: u64) -> Snapshot {
    let mut bids: BTreeMap<i64, f64> = BTreeMap::new();
    let mut asks: BTreeMap<i64, f64> = BTreeMap::new();

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

// ---------- chart + mode settings ----------

#[derive(Clone)]
struct ChartSettings {
    show_candles: usize,
    auto_y: bool,
    y_min: f64,
    y_max: f64,
    tf: u64,
}

impl Default for ChartSettings {
    fn default() -> Self {
        Self {
            show_candles: 200,
            auto_y: true,
            y_min: 0.0,
            y_max: 0.0,
            tf: 60,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Live,
    Replay,
}

// ---------- script defaults ----------

fn default_rhai_script() -> String {
    r#"
// ===== Orderbook Bubble Detector Bot =====
//
// Reads:
//   ctx["mode"], ctx["ticker"], ctx["mid"], ctx["last_vol"]
//   ctx["best_bid"], ctx["best_ask"]
//   ctx["bid_wall_price"], ctx["bid_wall_size"], ctx["bid_wall_score"]
//   ctx["ask_wall_price"], ctx["ask_wall_size"], ctx["ask_wall_score"]
//
// Controls:
//   auto_y, show_depth, show_ladders, show_trades, show_volume, reload_secs
//
// Emits:
//   bot_signal, bot_size, bot_comment
// =========================================

let mode      = ctx["mode"];
let ticker    = ctx["ticker"];
let mid       = ctx["mid"];
let last_vol  = ctx["last_vol"];

let best_bid       = ctx["best_bid"];
let best_ask       = ctx["best_ask"];
let bid_wp         = ctx["bid_wall_price"];
let bid_ws         = ctx["bid_wall_size"];
let bid_score      = ctx["bid_wall_score"];
let ask_wp         = ctx["ask_wall_price"];
let ask_ws         = ctx["ask_wall_size"];
let ask_score      = ctx["ask_wall_score"];

// defaults
auto_y      = true;
show_depth   = true;
show_ladders = true;
show_trades  = true;
show_volume  = true;
reload_secs  = 2.0;

bot_signal  = "FLAT";
bot_size    = 0.0;
bot_comment = "idle";

if mode != "live" {
    bot_comment = "not live mode";
    return;
}

if mid <= 0.0 || best_bid <= 0.0 || best_ask <= 0.0 {
    bot_comment = "bad mid / best prices";
    return;
}

// parameters
let score_th    = 2.5;
let base_min_sz = 5.0;
let max_dist_pct = 0.2;

let bid_min_sz = base_min_sz;
let ask_min_sz = base_min_sz;

if ticker == "BTC-USD" {
    bid_min_sz = 0.01;
    ask_min_sz = 0.01;
} else if ticker == "ETH-USD" {
    bid_min_sz = 0.1;
    ask_min_sz = 0.1;
} else if ticker == "SOL-USD" {
    bid_min_sz = 5.0;
    ask_min_sz = 5.0;
}

fn dist_pct(price, mid) {
    if mid <= 0.0 { return 9999.0; }
    return (price - mid).abs() * 100.0 / mid;
}

let bid_pct = dist_pct(bid_wp, mid);
let ask_pct = dist_pct(ask_wp, mid);

let has_bid = bid_score >= score_th && bid_ws >= bid_min_sz && bid_pct <= max_dist_pct;
let has_ask = ask_score >= score_th && ask_ws >= ask_min_sz && ask_pct <= max_dist_pct;

if !has_bid && !has_ask {
    bot_signal  = "FLAT";
    bot_size    = 0.0;
    bot_comment = "no strong bubbles";
    return;
}

let sug_size = if ticker == "BTC-USD" {
    0.002
} else if ticker == "ETH-USD" {
    0.05
} else if ticker == "SOL-USD" {
    2.0
} else {
    1.0
};

if has_bid && has_ask {
    bot_signal = "BOTH_BUBBLES";
    bot_size   = 0.0;
    bot_comment = `Both sides bubble: BID @ ${bid_wp} (size=${bid_ws},score=${bid_score}), \
ASK @ ${ask_wp} (size=${ask_ws},score=${ask_score}), mid=${mid}`;
    return;
}

if has_bid {
    bot_signal = "BID_BUBBLE";
    bot_size   = sug_size;
    bot_comment = `BID bubble @ ${bid_wp} (size=${bid_ws},score=${bid_score},dist=${bid_pct}%), mid=${mid}`;
    return;
}

if has_ask {
    bot_signal = "ASK_BUBBLE";
    bot_size   = sug_size;
    bot_comment = `ASK bubble @ ${ask_wp} (size=${ask_ws},score=${ask_score},dist=${ask_pct}%), mid=${mid}`;
    return;
}
"#
    .to_string()
}

// ---------- main app ----------

struct ComboApp {
    base_dir: PathBuf,
    tickers: Vec<String>,
    time_mode: TimeDisplayMode,
    mode: Mode,
    chart: ChartSettings,

    ticker_data: HashMap<String, TickerData>,
    current_ticker: String,

    live_ts: u64,
    replay_ts: u64,
    live_snapshot: Option<Snapshot>,
    replay_snapshot: Option<Snapshot>,

    last_reload: Instant,
    reload_secs: f64,

    show_depth: bool,
    show_ladders: bool,
    show_trades: bool,
    show_volume: bool,

    engine: Engine,
    scope: Scope<'static>,
    script_text: String,
    script_auto_run: bool,
    script_last_error: String,

    bot_signal: String,
    bot_size: f64,
    bot_comment: String,
}

impl ComboApp {
    fn new(base_dir: PathBuf) -> Self {
        let tickers = vec![
            "ETH-USD".to_string(),
            "BTC-USD".to_string(),
            "SOL-USD".to_string(),
        ];

        let mut ticker_data = HashMap::new();
        let mut global_min_ts = u64::MAX;
        let mut global_max_ts = 0u64;

        for tk in &tickers {
            if let Some(td) = load_ticker_data(&base_dir, tk) {
                global_min_ts = min(global_min_ts, td.min_ts);
                global_max_ts = max(global_max_ts, td.max_ts);
                ticker_data.insert(tk.clone(), td);
            }
        }

        let current_ticker = tickers[0].clone();

        let (live_ts, replay_ts) = if global_min_ts == u64::MAX {
            (0, 0)
        } else {
            (global_max_ts, global_max_ts)
        };

        let engine = Engine::new();
        let scope = Scope::new();

        let mut app = Self {
            base_dir,
            tickers,
            time_mode: TimeDisplayMode::Local,
            mode: Mode::Live,
            chart: ChartSettings::default(),
            ticker_data,
            current_ticker,
            live_ts,
            replay_ts,
            live_snapshot: None,
            replay_snapshot: None,
            last_reload: Instant::now(),
            reload_secs: 2.0,
            show_depth: true,
            show_ladders: true,
            show_trades: true,
            show_volume: true,
            engine,
            scope,
            script_text: default_rhai_script(),
            script_auto_run: true,
            script_last_error: String::new(),
            bot_signal: "FLAT".to_string(),
            bot_size: 0.0,
            bot_comment: "idle".to_string(),
        };

        app.refresh_snapshots();
        app
    }

    fn current_ticker_data(&self) -> Option<&TickerData> {
        self.ticker_data.get(&self.current_ticker)
    }

    fn clamp_ts(&mut self) {
        let td_opt = self
            .current_ticker_data()
            .map(|td| (td.min_ts, td.max_ts));
        let Some((min_ts, max_ts)) = td_opt else {
            return;
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

    fn refresh_snapshots(&mut self) {
        self.clamp_ts();

        let td_opt = self.current_ticker_data().cloned();
        if let Some(td) = td_opt {
            self.live_snapshot = Some(compute_snapshot_for(&td, self.live_ts));
            self.replay_snapshot =
                Some(compute_snapshot_for(&td, self.replay_ts));
        } else {
            self.live_snapshot = None;
            self.replay_snapshot = None;
        }
    }

    fn reload_csv_if_due(&mut self) {
        let now = Instant::now();
        if now.duration_since(self.last_reload).as_secs_f64() < self.reload_secs {
            return;
        }

        self.last_reload = now;

        let mut new_data = HashMap::new();
        let mut global_min_ts = u64::MAX;
        let mut global_max_ts = 0u64;

        for tk in &self.tickers {
            if let Some(td) = load_ticker_data(&self.base_dir, tk) {
                global_min_ts = min(global_min_ts, td.min_ts);
                global_max_ts = max(global_max_ts, td.max_ts);
                new_data.insert(tk.clone(), td);
            }
        }

        if !new_data.is_empty() {
            self.ticker_data = new_data;
            if global_max_ts != 0 {
                if self.live_ts == 0 || self.live_ts == global_max_ts {
                    self.live_ts = global_max_ts;
                }
                if self.replay_ts == 0 {
                    self.replay_ts = global_max_ts;
                }
            }
            self.refresh_snapshots();
        }
    }

    fn series_from_snap<'a>(&self, snap: &'a Snapshot) -> &'a [Candle] {
        match self.chart.tf {
            30 => &snap.tf_30s,
            60 => &snap.tf_1m,
            180 => &snap.tf_3m,
            300 => &snap.tf_5m,
            _ => &snap.tf_1m,
        }
    }

    fn run_script_for_snap(&mut self, snap: &Snapshot, mode_str: &str) {
        let bubbles = compute_bubbles(&snap.bids, &snap.asks, 25);

        let mut ctx_map = RhaiMap::new();
        ctx_map.insert("mode".into(), mode_str.into());
        ctx_map
            .insert("ticker".into(), self.current_ticker.clone().into());
        ctx_map.insert("mid".into(), snap.last_mid.into());
        ctx_map.insert("last_vol".into(), snap.last_vol.into());

        ctx_map.insert("best_bid".into(), bubbles.best_bid.into());
        ctx_map.insert("best_ask".into(), bubbles.best_ask.into());
        ctx_map.insert("bid_wall_price".into(), bubbles.bid_wall_price.into());
        ctx_map.insert("bid_wall_size".into(), bubbles.bid_wall_size.into());
        ctx_map.insert("bid_wall_score".into(), bubbles.bid_wall_score.into());
        ctx_map.insert("ask_wall_price".into(), bubbles.ask_wall_price.into());
        ctx_map.insert("ask_wall_size".into(), bubbles.ask_wall_size.into());
        ctx_map.insert("ask_wall_score".into(), bubbles.ask_wall_score.into());

        self.scope.clear();

        self.scope.push("ctx", ctx_map);

        self.scope.push("auto_y", self.chart.auto_y);
        self.scope.push("show_depth", self.show_depth);
        self.scope.push("show_ladders", self.show_ladders);
        self.scope.push("show_trades", self.show_trades);
        self.scope.push("show_volume", self.show_volume);
        self.scope.push("reload_secs", self.reload_secs);

        self.scope.push("bot_signal", self.bot_signal.clone());
        self.scope.push("bot_size", self.bot_size);
        self.scope
            .push("bot_comment", self.bot_comment.clone());

        if let Err(e) = self
            .engine
            .eval_with_scope::<()>(&mut self.scope, &self.script_text)
        {
            self.script_last_error = e.to_string();
            return;
        } else {
            self.script_last_error.clear();
        }

        if let Some(v) = self.scope.get_value::<bool>("auto_y") {
            self.chart.auto_y = v;
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
        if let Some(v) = self.scope.get_value::<f64>("reload_secs") {
            self.reload_secs = v.clamp(0.5, 60.0);
        }

        if let Some(v) = self.scope.get_value::<String>("bot_signal") {
            self.bot_signal = v;
        }
        if let Some(v) = self.scope.get_value::<f64>("bot_size") {
            self.bot_size = v.max(0.0);
        }
        if let Some(v) = self.scope.get_value::<String>("bot_comment") {
            self.bot_comment = v;
        }
    }

    fn ui_top_bar(&mut self, ui: &mut egui::Ui) {
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
                            self.refresh_snapshots();
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

            if let Some(td) = self.current_ticker_data() {
                ui.separator();
                ui.label(format!(
                    "Range: {} → {}",
                    format_ts(self.time_mode, td.min_ts),
                    format_ts(self.time_mode, td.max_ts)
                ));
            }

            ui.separator();
            ui.label("TF:");
            for (label, tf) in [("30s", 30u64), ("1m", 60), ("3m", 180), ("5m", 300)] {
                if ui
                    .selectable_label(self.chart.tf == tf, label)
                    .clicked()
                {
                    self.chart.tf = tf;
                }
            }

            ui.separator();
            ui.label("Bot:");
            ui.label(format!(
                "{} (size {:.6})",
                self.bot_signal, self.bot_size
            ));
        });

        // Replay slider
        if matches!(self.mode, Mode::Replay) {
            if let Some(td) = self.current_ticker_data() {
                let min_ts = td.min_ts;
                let max_ts = td.max_ts;
                let mut ts = self.replay_ts;

                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("Replay ts:");
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

                if let Some(td2) = self.current_ticker_data() {
                    self.replay_snapshot =
                        Some(compute_snapshot_for(td2, self.replay_ts));
                }
            }
        }
    }

    fn ui_script_panel(&mut self, ui: &mut egui::Ui) {
        ui.collapsing("Script Engine (Rhai)", |ui| {
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.script_auto_run, "Auto-run");
                if ui.button("Run now").clicked() {
                    let snap_opt = match self.mode {
                        Mode::Live => self.live_snapshot.clone(),
                        Mode::Replay => self.replay_snapshot.clone(),
                    };
                    if let Some(snap) = snap_opt {
                        let mode_str = if matches!(self.mode, Mode::Live) {
                            "live"
                        } else {
                            "replay"
                        };
                        self.run_script_for_snap(&snap, mode_str);
                    }
                }

                if ui.button("Reset script").clicked() {
                    self.script_text = default_rhai_script();
                }
            });

            if !self.script_last_error.is_empty() {
                ui.colored_label(
                    Color32::RED,
                    format!("Script error: {}", self.script_last_error),
                );
            }

            ui.separator();
            egui::ScrollArea::vertical()
                .max_height(200.0)
                .show(ui, |ui| {
                    ui.code_editor(&mut self.script_text);
                });

            ui.separator();
            ui.label(format!(
                "Bot signal: {}, size: {:.6}",
                self.bot_signal, self.bot_size
            ));
            ui.label(format!("Bot comment: {}", self.bot_comment));
        });
    }

    fn ui_depth_plot(&self, ui: &mut egui::Ui, snap: &Snapshot, height: f32) {
        if !self.show_depth {
            return;
        }

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
            .show(ui, |plot_ui| {
                if !bid_points.is_empty() {
                    let pts: PlotPoints = bid_points
                        .iter()
                        .map(|(x, y)| [*x, *y])
                        .collect::<Vec<_>>()
                        .into();
                    plot_ui
                        .line(Line::new(pts).name("Bids").color(Color32::GREEN));
                }
                if !ask_points.is_empty() {
                    let pts: PlotPoints = ask_points
                        .iter()
                        .map(|(x, y)| [*x, *y])
                        .collect::<Vec<_>>()
                        .into();
                    plot_ui
                        .line(Line::new(pts).name("Asks").color(Color32::RED));
                }
            });
    }

    fn ui_ladders_and_trades(&self, ui: &mut egui::Ui, snap: &Snapshot) {
        if !self.show_ladders && !self.show_trades {
            return;
        }

        ui.horizontal(|ui| {
            if self.show_ladders {
                ui.vertical(|ui| {
                    ui.label("Bids (top 20)");
                    egui::Grid::new("bids_grid")
                        .striped(true)
                        .show(ui, |ui| {
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

                    ui.separator();
                    ui.label("Asks (top 20)");
                    egui::Grid::new("asks_grid")
                        .striped(true)
                        .show(ui, |ui| {
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
            }

            if self.show_trades {
                ui.separator();
                ui.vertical(|ui| {
                    ui.label("Recent trades");
                    egui::ScrollArea::vertical()
                        .max_height(200.0)
                        .show(ui, |ui| {
                            egui::Grid::new("trades_grid")
                                .striped(true)
                                .show(ui, |ui| {
                                    ui.label("Time");
                                    ui.label("Side");
                                    ui.label("Size");
                                    ui.end_row();

                                    for tr in snap.trades.iter().rev() {
                                        ui.label(format_ts(
                                            self.time_mode,
                                            tr.ts,
                                        ));
                                        ui.label(&tr.side);
                                        ui.label(&tr.size_str);
                                        ui.end_row();
                                    }
                                });
                        });
                });
            }
        });
    }

    fn ui_candles_and_volume(
        &mut self,
        ui: &mut egui::Ui,
        snap: &Snapshot,
        is_live: bool,
    ) {
        let series = self.series_from_snap(snap);
        if series.is_empty() {
            ui.label(if is_live {
                "No candles yet (live)."
            } else {
                "No candles yet (replay)."
            });
            return;
        }

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

        let avail_w = ui.available_width();
        let avail_h = ui.available_height();
        let candles_h = avail_h * 0.7;
        let volume_h = avail_h * 0.3;

        let tf = self.chart.tf as f64;
        let last = visible.last().unwrap();
        let x_center = last.t as f64 + tf * 0.5;
        let span = tf * (self.chart.show_candles as f64);
        let x_min = x_center - span * 0.5;
        let x_max = x_center + span * 0.5;

        let time_mode = self.time_mode;
        let now_x = if is_live {
            self.live_ts as f64
        } else {
            self.replay_ts as f64
        };

        // candles
        ui.allocate_ui(egui::vec2(avail_w, candles_h), |ui| {
            Plot::new(if is_live {
                "candles_live"
            } else {
                "candles_replay"
            })
            .height(candles_h)
            .include_y(y_min)
            .include_y(y_max)
            .allow_drag(true)
            .allow_zoom(true)
            .x_axis_formatter(move |mark, _, _| {
                let ts = mark.value as u64;
                format_ts(time_mode, ts)
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

                    let wick_pts: PlotPoints =
                        vec![[mid, c.low], [mid, c.high]].into();
                    plot_ui.line(Line::new(wick_pts).color(color));

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

                plot_ui.vline(VLine::new(now_x).color(Color32::YELLOW));
            });
        });

        ui.separator();

        if self.show_volume {
            let time_mode2 = self.time_mode;
            ui.allocate_ui(egui::vec2(avail_w, volume_h), |ui| {
                Plot::new(if is_live { "vol_live" } else { "vol_replay" })
                    .height(volume_h)
                    .include_y(0.0)
                    .allow_drag(true)
                    .allow_zoom(true)
                    .x_axis_formatter(move |mark, _, _| {
                        let ts = mark.value as u64;
                        format_ts(time_mode2, ts)
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
                            let pts: PlotPoints =
                                vec![[mid, 0.0], [mid, c.volume]].into();
                            plot_ui
                                .line(Line::new(pts).color(color).width(2.0));
                        }
                    });
            });
        }
    }

    fn ui_live(&mut self, ui: &mut egui::Ui) {
        if let Some(snap) = self.live_snapshot.clone() {
            ui.heading(format!(
                "LIVE {} @ {}",
                self.current_ticker,
                format_ts(self.time_mode, self.live_ts)
            ));

            if self.script_auto_run {
                self.run_script_for_snap(&snap, "live");
            }

            ui.separator();

            let avail_w = ui.available_width();
            let avail_h = ui.available_height();
            let top_h = avail_h * 0.4;
            let bot_h = avail_h * 0.6;

            ui.allocate_ui(egui::vec2(avail_w, top_h), |ui| {
                ui.columns(2, |cols| {
                    self.ui_depth_plot(&mut cols[0], &snap, top_h * 0.9);
                    self.ui_ladders_and_trades(&mut cols[1], &snap);
                });
            });

            ui.separator();

            ui.allocate_ui(egui::vec2(avail_w, bot_h), |ui| {
                self.ui_candles_and_volume(ui, &snap, true);
            });
        } else {
            ui.label("No live snapshot for this ticker yet. Check CSV files.");
        }
    }

    fn ui_replay(&mut self, ui: &mut egui::Ui) {
        if let Some(snap) = self.replay_snapshot.clone() {
            ui.heading(format!(
                "REPLAY {} @ {}",
                self.current_ticker,
                format_ts(self.time_mode, self.replay_ts)
            ));

            if self.script_auto_run {
                self.run_script_for_snap(&snap, "replay");
            }

            ui.separator();

            let avail_w = ui.available_width();
            let avail_h = ui.available_height();
            let top_h = avail_h * 0.4;
            let bot_h = avail_h * 0.6;

            ui.allocate_ui(egui::vec2(avail_w, top_h), |ui| {
                ui.columns(2, |cols| {
                    self.ui_depth_plot(&mut cols[0], &snap, top_h * 0.9);
                    self.ui_ladders_and_trades(&mut cols[1], &snap);
                });
            });

            ui.separator();

            ui.allocate_ui(egui::vec2(avail_w, bot_h), |ui| {
                self.ui_candles_and_volume(ui, &snap, false);
            });
        } else {
            ui.label("No replay snapshot. Check CSV for this ticker.");
        }
    }
}

impl eframe::App for ComboApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.reload_csv_if_due();

        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            self.ui_top_bar(ui);
        });

        egui::TopBottomPanel::bottom("script_panel")
            .resizable(true)
            .show(ctx, |ui| {
                self.ui_script_panel(ui);
            });

        egui::CentralPanel::default().show(ctx, |ui| match self.mode {
            Mode::Live => self.ui_live(ui),
            Mode::Replay => self.ui_replay(ui),
        });

        ctx.request_repaint_after(Duration::from_millis(50));
    }
}

// ---------- main ----------

fn main() {
    let base_dir = PathBuf::from("data");
    let app = ComboApp::new(base_dir);

    let options = eframe::NativeOptions::default();

    if let Err(e) =
        eframe::run_native("dYdX CSV GUI + Rhai Bot", options, Box::new(|_| Box::new(app)))
    {
        eprintln!("eframe error: {e}");
    }
}
