// ladder_app/src/bin/full_gui_x12.rs
//
// Visual frontend for dYdX data_daemon CSVs with:
// - 3 x 6 flexible grid layout (per-row layout + adjustable ratios)
// - Live + Replay modes, reading only from daemon CSVs in ./data
// - Ticker selection: ETH-USD, BTC-USD, SOL-USD
// - Timeframes from 1s to 1d (candles generated from CSV orderbook midprices)
// - Separate Candles and Volume panels
// - Depth, Ladders + Trades, Summary panels
// - Script engine panel (Rhai) to control settings and emit bot signals
//
// This expects the daemon to be writing:
//   data/orderbook_{TICKER}.csv
//       ts,ticker,kind,side,price,size
//   data/trades_{TICKER}.csv
//       ts,ticker,source,side,size_str
//
// and does not connect directly to dYdX indexer/node.
//
// IMPORTANT: This is a self-contained UI file. It assumes you have:
//   mod candle_agg;
// with
//   pub struct Candle { pub t: u64, pub open: f64, pub high: f64, pub low: f64, pub close: f64, pub volume: f64 }
//   pub struct CandleAgg::new(tf_secs: u64)
//   fn update(ts: u64, mid: f64, vol: f64)
//   fn series(&self) -> &[Candle]

mod candle_agg;

use candle_agg::{Candle, CandleAgg};

use chrono::{Local, TimeZone};
use eframe::egui::{self, Color32};
use egui_plot::{Line, Plot, PlotBounds, PlotPoints, VLine};

use rhai::{Dynamic, Engine, Map as RhaiMap, Scope};

use std::cmp::{max, min};
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// ------------ basic helpers ------------

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_secs()
}

// integer price key so BTreeMap sorts nicely
type PriceKey = i64;

fn price_to_key(price: f64) -> PriceKey {
    (price * 10_000.0).round() as PriceKey
}

fn key_to_price(key: PriceKey) -> f64 {
    key as f64 / 10_000.0
}

// ------------ time formatting ------------

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

// ------------ chart / layout settings ------------

/// Available timeframes in seconds, from 1s to 1d.
const TF_LIST: &[(i64, &str)] = &[
    (1, "1s"),
    (5, "5s"),
    (15, "15s"),
    (30, "30s"),
    (60, "1m"),
    (120, "2m"),
    (300, "5m"),
    (900, "15m"),
    (1800, "30m"),
    (3600, "1h"),
    (7200, "2h"),
    (14400, "4h"),
    (28800, "8h"),
    (86400, "1d"),
];

#[derive(Clone)]
struct ChartSettings {
    tf: i64,
    show_candles: usize,
    auto_y: bool,
    y_min: f64,
    y_max: f64,
    x_zoom: f64,
    x_pan_secs: f64,
}

impl Default for ChartSettings {
    fn default() -> Self {
        Self {
            tf: 60,
            show_candles: 200,
            auto_y: true,
            y_min: 0.0,
            y_max: 0.0,
            x_zoom: 1.0,
            x_pan_secs: 0.0,
        }
    }
}

// grid config: 3 wide x 6 tall
const COLS: usize = 3;
const ROWS: usize = 6;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RowLayout {
    Three,   // 3 equal cells
    Two12,   // left big (cols 1+2), right small (col 3)
    Two23,   // left small (col 1), right big (cols 2+3)
    One123,  // single full-width cell
}

impl RowLayout {
    fn label(self) -> &'static str {
        match self {
            RowLayout::Three => "3 cells (equal)",
            RowLayout::Two12 => "2 cells (Left big)",
            RowLayout::Two23 => "2 cells (Right big)",
            RowLayout::One123 => "1 cell (full row)",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CellKind {
    Empty,
    Summary,
    Candles,
    Volume,
    Depth,
    LaddersTrades,
    ScriptEngine,
    TradingPanel,
}

impl CellKind {
    fn label(self) -> &'static str {
        match self {
            CellKind::Empty => "Empty",
            CellKind::Summary => "Summary",
            CellKind::Candles => "Candles",
            CellKind::Volume => "Volume",
            CellKind::Depth => "Depth",
            CellKind::LaddersTrades => "Ladders + Trades",
            CellKind::ScriptEngine => "Script Engine",
            CellKind::TradingPanel => "Trading Panel",
        }
    }

    fn all() -> &'static [CellKind] {
        &[
            CellKind::Empty,
            CellKind::Summary,
            CellKind::Candles,
            CellKind::Volume,
            CellKind::Depth,
            CellKind::LaddersTrades,
            CellKind::ScriptEngine,
            CellKind::TradingPanel,
        ]
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Live,
    Replay,
}

// ------------ CSV + replay structures ------------

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
    /// Candles per timeframe (seconds -> Vec<Candle>)
    candles: HashMap<i64, Vec<Candle>>,
    last_mid: f64,
    last_vol: f64,
    trades: Vec<TradeCsvEvent>,
}

// --- CSV IO ---

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
    let mut bids: BTreeMap<PriceKey, f64> = BTreeMap::new();
    let mut asks: BTreeMap<PriceKey, f64> = BTreeMap::new();

    // Build multiple aggregators for all requested TFs
    let mut aggs: HashMap<i64, CandleAgg> = TF_LIST
        .iter()
        .map(|(tf, _)| (*tf, CandleAgg::new(*tf as u64)))
        .collect();

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
            for (_tf, agg) in aggs.iter_mut() {
                agg.update(e.ts, mid, vol);
            }
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

    let mut candles: HashMap<i64, Vec<Candle>> = HashMap::new();
    for (tf, agg) in aggs {
        candles.insert(tf, agg.series().to_vec());
    }

    let last_mid_vol = candles
        .get(&60)
        .and_then(|v| v.last())
        .map(|c| (c.close, c.volume))
        .unwrap_or((0.0, 0.0));

    Snapshot {
        bids,
        asks,
        candles,
        last_mid: last_mid_vol.0,
        last_vol: last_mid_vol.1,
        trades,
    }
}

// ------------ app struct ------------

struct ComboApp {
    base_dir: PathBuf,

    tickers: Vec<String>,
    current_ticker: String,

    mode: Mode,
    time_mode: TimeDisplayMode,
    chart: ChartSettings,

    row_layouts: [RowLayout; ROWS],
    row_ratios: [f32; ROWS], // 0.2..0.8 for 2-cell layouts
    cell_kinds: [CellKind; ROWS * COLS],

    // time positions
    live_ts: u64,
    replay_ts: u64,

    ticker_data: HashMap<String, TickerData>,
    live_snapshot: Option<Snapshot>,
    replay_snapshot: Option<Snapshot>,

    // script engine state
    engine: Engine,
    scope: Scope<'static>,
    script_source: String,
    script_last_error: Option<String>,
    script_last_info: String,
    bot_signal: Option<String>,
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
        let mut live_ts = now_unix();
        let mut replay_ts = now_unix();
        let mut live_snapshot = None;
        let mut replay_snapshot = None;

        for tk in &tickers {
            if let Some(td) = load_ticker_data(&base_dir, tk) {
                live_ts = td.max_ts;
                replay_ts = td.min_ts;
                if live_snapshot.is_none() {
                    live_snapshot = Some(compute_snapshot_for(&td, live_ts));
                    replay_snapshot = Some(compute_snapshot_for(&td, replay_ts));
                }
                ticker_data.insert(tk.clone(), td);
            }
        }

        // default: ETH-USD if present
        let current_ticker = tickers
            .iter()
            .find(|t| ticker_data.contains_key(*t))
            .cloned()
            .unwrap_or_else(|| "ETH-USD".to_string());

        let row_layouts = [RowLayout::Three; ROWS];
        let row_ratios = [0.66; ROWS];

        // initial cell contents
        let mut cell_kinds = [CellKind::Empty; ROWS * COLS];
        // Row 0: summary | candles | depth
        cell_kinds[0] = CellKind::Summary;
        cell_kinds[1] = CellKind::Candles;
        cell_kinds[2] = CellKind::Depth;
        // Row 1: volume | ladders | trades
        cell_kinds[3] = CellKind::Volume;
        cell_kinds[4] = CellKind::LaddersTrades;
        // Row 2: script engine full row
        // (user can change later)
        cell_kinds[6] = CellKind::ScriptEngine;
        // Row 3: trading panel
        cell_kinds[9] = CellKind::TradingPanel;

        let mut engine = Engine::new();
        engine.set_max_modules(8);
        engine.set_max_expr_depths(32, 32);
        engine.set_max_operations(200_000);

        let scope = Scope::new();

        Self {
            base_dir,
            tickers,
            current_ticker,
            mode: Mode::Live,
            time_mode: TimeDisplayMode::Local,
            chart: ChartSettings::default(),
            row_layouts,
            row_ratios,
            cell_kinds,
            live_ts,
            replay_ts,
            ticker_data,
            live_snapshot,
            replay_snapshot,
            engine,
            scope,
            script_source: String::from(
                "// Example bot script (Rhai)
// Available: mid, best_bid, best_ask, tf, history
// Return a map: #{ tf: 60, history: 300, auto_y: true, signal: \"none\", size: 0.0 }

let out = #{};

// change tf based on volatility
if mid > 0.0 {
    out.tf = 60;
    out.history = 300;
    out.auto_y = true;
}

// simple bubble detector: big gap between best bid and best ask
if best_ask - best_bid > mid * 0.001 {
    out.signal = \"bubble\";
    out.size = 0.01;
    out.comment = \"Spread wider than 0.1%\";    
}

out;",
            ),
            script_last_error: None,
            script_last_info: String::new(),
            bot_signal: None,
            bot_size: 0.0,
            bot_comment: String::new(),
        }
    }

    fn current_td_clone(&self) -> Option<TickerData> {
        self.ticker_data.get(&self.current_ticker).cloned()
    }

    fn refresh_from_csv(&mut self) {
        if let Some(td) = load_ticker_data(&self.base_dir, &self.current_ticker) {
            self.live_ts = td.max_ts;
            self.replay_ts = td.min_ts;
            self.live_snapshot = Some(compute_snapshot_for(&td, self.live_ts));
            self.replay_snapshot = Some(compute_snapshot_for(&td, self.replay_ts));
            self.ticker_data.insert(self.current_ticker.clone(), td);
        }
    }

    fn ensure_replay_ts_in_range(&mut self) {
        if let Some(td) = self.current_td_clone() {
            if self.replay_ts < td.min_ts {
                self.replay_ts = td.min_ts;
            }
            if self.replay_ts > td.max_ts {
                self.replay_ts = td.max_ts;
            }
        }
    }

    fn snapshot_for_mode(&self) -> Option<&Snapshot> {
        match self.mode {
            Mode::Live => self.live_snapshot.as_ref(),
            Mode::Replay => self.replay_snapshot.as_ref(),
        }
    }

    fn series_from_snap<'a>(&self, snap: &'a Snapshot) -> &'a [Candle] {
        if let Some(v) = snap.candles.get(&self.chart.tf) {
            v.as_slice()
        } else if let Some((_, v)) = snap.candles.iter().next() {
            v.as_slice()
        } else {
            &[]
        }
    }

    // ------------- script engine -------------

    fn run_script_engine(&mut self, snap_opt: Option<&Snapshot>) {
        let snap = match snap_opt {
            Some(s) => s,
            None => {
                self.script_last_error = Some("No snapshot for script".to_string());
                return;
            }
        };

        // compute some basic stats to feed into script
        let (mid, best_bid, best_ask) = {
            let mut mid = 0.0;
            let mut bb = 0.0;
            let mut ba = 0.0;

            if let (Some((b_key, _)), Some((a_key, _))) =
                (snap.bids.iter().next_back(), snap.asks.iter().next())
            {
                bb = key_to_price(*b_key);
                ba = key_to_price(*a_key);
                mid = (bb + ba) * 0.5;
            }
            (mid, bb, ba)
        };

        let history = self.chart.show_candles as i64;
        let tf = self.chart.tf;

        self.scope.clear();
        self.scope.push("mid", mid);
        self.scope.push("best_bid", best_bid);
        self.scope.push("best_ask", best_ask);
        self.scope.push("tf", tf);
        self.scope.push("history", history);

        let script = self.script_source.clone();

        match self
            .engine
            .eval_with_scope::<Dynamic>(&mut self.scope, &script)
        {
            Ok(dynv) => {
                self.script_last_error = None;
                if let Some(map) = dynv.clone().try_cast::<RhaiMap>() {
                    self.apply_script_map(&map);
                    self.script_last_info =
                        format!("Script OK, map keys: {:?}", map.keys().collect::<Vec<_>>());
                } else {
                    self.script_last_info =
                        "Script ran but did not return a map; no changes applied".to_string();
                }
            }
            Err(e) => {
                self.script_last_error = Some(format!("{e}"));
            }
        }
    }

    fn apply_script_map(&mut self, m: &RhaiMap) {
        // helper macro
        macro_rules! set_if {
            ($key:expr, $ty:ty, $target:expr) => {
                if let Some(v_dyn) = m.get($key) {
                    if let Some(v) = v_dyn.clone().try_cast::<$ty>() {
                        $target = v;
                    }
                }
            };
        }

        // tf + history + auto_y
        set_if!("tf", i64, self.chart.tf);
        if self.chart.tf <= 0 {
            self.chart.tf = 60;
        }

        if let Some(v_dyn) = m.get("history") {
            if let Some(v) = v_dyn.clone().try_cast::<i64>() {
                self.chart.show_candles = v.clamp(20, 2000) as usize;
            }
        }

        if let Some(v_dyn) = m.get("auto_y") {
            if let Some(v) = v_dyn.clone().try_cast::<bool>() {
                self.chart.auto_y = v;
            }
        }

        // Optional y_min / y_max overrides
        if let Some(v_dyn) = m.get("y_min") {
            if let Some(v) = v_dyn.clone().try_cast::<f64>() {
                self.chart.y_min = v;
                self.chart.auto_y = false;
            }
        }
        if let Some(v_dyn) = m.get("y_max") {
            if let Some(v) = v_dyn.clone().try_cast::<f64>() {
                self.chart.y_max = v;
                self.chart.auto_y = false;
            }
        }

        // bot signal fields
        self.bot_signal = None;
        self.bot_size = 0.0;
        self.bot_comment.clear();

        if let Some(sig_dyn) = m.get("signal") {
            if let Some(sig) = sig_dyn.clone().try_cast::<String>() {
                if !sig.is_empty() && sig != "none" {
                    self.bot_signal = Some(sig);
                }
            }
        }
        if let Some(size_dyn) = m.get("size") {
            if let Some(size) = size_dyn.clone().try_cast::<f64>() {
                self.bot_size = size;
            }
        }
        if let Some(comment_dyn) = m.get("comment") {
            if let Some(comment) = comment_dyn.clone().try_cast::<String>() {
                self.bot_comment = comment;
            }
        }
    }

    // ------------- UI helpers -------------

    fn ui_top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            // mode
            ui.label("Mode:");
            if ui
                .selectable_label(matches!(self.mode, Mode::Live), "Live")
                .clicked()
            {
                self.mode = Mode::Live;
            }
            if ui
                .selectable_label(matches!(self.mode, Mode::Replay), "Replay")
                .clicked()
            {
                self.mode = Mode::Replay;
                self.ensure_replay_ts_in_range();
            }

            ui.separator();

            // ticker
            ui.menu_button(format!("Ticker: {}", self.current_ticker), |ui| {
                for t in &self.tickers {
                    let selected = *t == self.current_ticker;
                    if ui.selectable_label(selected, t).clicked() {
                        self.current_ticker = t.clone();
                        // reload data for newly selected ticker
                        if let Some(td) = load_ticker_data(&self.base_dir, t) {
                            self.live_ts = td.max_ts;
                            self.replay_ts = td.min_ts;
                            self.live_snapshot =
                                Some(compute_snapshot_for(&td, self.live_ts));
                            self.replay_snapshot =
                                Some(compute_snapshot_for(&td, self.replay_ts));
                            self.ticker_data.insert(t.clone(), td);
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

            if let Some(td) = self.current_td_clone() {
                ui.separator();
                ui.label(format!(
                    "Range: {} → {}",
                    format_ts(self.time_mode, td.min_ts),
                    format_ts(self.time_mode, td.max_ts)
                ));
            }

            if let Some(snap) = self.snapshot_for_mode() {
                ui.separator();
                ui.label(format!("Mid: {:.2}", snap.last_mid));
                ui.label(format!("Last vol: {:.4}", snap.last_vol));
            }
        });

        ui.separator();

        // per-mode extra controls
        if let Some(td) = self.current_td_clone() {
            match self.mode {
                Mode::Live => {
                    ui.horizontal(|ui| {
                        ui.label("Live ts:");
                        ui.label(format_ts(self.time_mode, self.live_ts));
                        if ui.button("Refresh from CSV").clicked() {
                            self.refresh_from_csv();
                        }
                        if ui.button("Jump to latest").clicked() {
                            self.live_ts = td.max_ts;
                            self.live_snapshot =
                                Some(compute_snapshot_for(&td, self.live_ts));
                        }
                    });
                }
                Mode::Replay => {
                    ui.horizontal(|ui| {
                        let mut ts = self.replay_ts;
                        ui.label("Replay ts:");
                        ui.add(
                            egui::Slider::new(&mut ts, td.min_ts..=td.max_ts)
                                .show_value(false),
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
                        if ts != self.replay_ts {
                            self.replay_ts = ts;
                            self.replay_snapshot =
                                Some(compute_snapshot_for(&td, self.replay_ts));
                        }
                    });
                }
            }
        } else {
            ui.label("No CSV data for this ticker yet.");
        }

        ui.separator();

        // chart options
        ui.horizontal(|ui| {
            // timeframe combo
            ui.label("TF:");
            egui::ComboBox::from_id_source("tf_combo")
                .selected_text(
                    TF_LIST
                        .iter()
                        .find(|(v, _)| *v == self.chart.tf)
                        .map(|(_, name)| *name)
                        .unwrap_or("custom"),
                )
                .show_ui(ui, |ui| {
                    for (v, label) in TF_LIST {
                        if ui
                            .selectable_label(self.chart.tf == *v, *label)
                            .clicked()
                        {
                            self.chart.tf = *v;
                        }
                    }
                });

            ui.separator();

            ui.label("History candles:");
            ui.add(
                egui::Slider::new(&mut self.chart.show_candles, 20..=2000)
                    .logarithmic(true),
            );

            ui.separator();

            ui.label("X zoom:");
            ui.add(
                egui::Slider::new(&mut self.chart.x_zoom, 0.25..=8.0)
                    .logarithmic(true),
            );

            ui.separator();
            if ui.button("Center X").clicked() {
                self.chart.x_pan_secs = 0.0;
            }

            ui.separator();
            ui.checkbox(&mut self.chart.auto_y, "Auto Y");
            if !self.chart.auto_y {
                ui.label("Y range:");
                ui.add(egui::DragValue::new(&mut self.chart.y_min).speed(1.0));
                ui.add(egui::DragValue::new(&mut self.chart.y_max).speed(1.0));
            }
        });
    }

    fn ui_summary(&self, ui: &mut egui::Ui, snap_opt: Option<&Snapshot>) {
        ui.heading("Summary");
        if let Some(td) = self.current_td_clone() {
            ui.label(format!("Ticker: {}", self.current_ticker));
            ui.label(format!(
                "CSV range: {} → {}",
                format_ts(self.time_mode, td.min_ts),
                format_ts(self.time_mode, td.max_ts)
            ));
            ui.label(format!("Book events: {}", td.book_events.len()));
            ui.label(format!("Trade events: {}", td.trade_events.len()));
        } else {
            ui.label("No ticker data loaded.");
        }

        if let Some(snap) = snap_opt {
            ui.separator();
            ui.label(format!("Best bid: {:.4}", snap.bids.iter().next_back().map(|(k,_)| key_to_price(*k)).unwrap_or(0.0)));
            ui.label(format!("Best ask: {:.4}", snap.asks.iter().next().map(|(k,_)| key_to_price(*k)).unwrap_or(0.0)));
            ui.label(format!("Mid: {:.4}", snap.last_mid));
            ui.label(format!("Last vol (1m): {:.4}", snap.last_vol));
        }
    }

    fn ui_depth(&self, ui: &mut egui::Ui, snap_opt: Option<&Snapshot>) {
        ui.heading("Depth");
        let snap = match snap_opt {
            Some(s) => s,
            None => {
                ui.label("No snapshot.");
                return;
            }
        };

        let w = ui.available_width();
        let h = ui.available_height().max(150.0);

        ui.allocate_ui(egui::vec2(w, h), |ui| {
            let mut bid_points = Vec::new();
            let mut ask_points = Vec::new();

            let mut cum = 0.0;
            for (k, s) in snap.bids.iter().rev() {
                let p = key_to_price(*k);
                cum += s;
                bid_points.push([p, cum]);
            }

            cum = 0.0;
            for (k, s) in snap.asks.iter() {
                let p = key_to_price(*k);
                cum += s;
                ask_points.push([p, cum]);
            }

            Plot::new("depth_plot")
                .height(h * 0.95)
                .show(ui, |plot_ui| {
                    if !bid_points.is_empty() {
                        plot_ui.line(
                            Line::new(PlotPoints::from_iter(bid_points.into_iter()))
                                .color(Color32::from_rgb(80, 200, 120))
                                .name("Bids"),
                        );
                    }
                    if !ask_points.is_empty() {
                        plot_ui.line(
                            Line::new(PlotPoints::from_iter(ask_points.into_iter()))
                                .color(Color32::from_rgb(220, 80, 80))
                                .name("Asks"),
                        );
                    }
                });
        });
    }

    fn ui_ladders_trades(&self, ui: &mut egui::Ui, snap_opt: Option<&Snapshot>) {
        ui.heading("Ladders + Trades");
        let snap = match snap_opt {
            Some(s) => s,
            None => {
                ui.label("No snapshot.");
                return;
            }
        };

        egui::ScrollArea::both()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.columns(2, |cols| {
                    cols[0].label("Bids");
                    egui::Grid::new("bids_grid")
                        .striped(true)
                        .show(&mut cols[0], |ui| {
                            ui.label("Price");
                            ui.label("Size");
                            ui.end_row();
                            for (k, s) in snap.bids.iter().rev().take(40) {
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
                            for (k, s) in snap.asks.iter().take(40) {
                                let p = key_to_price(*k);
                                ui.label(format!("{:>9.2}", p));
                                ui.label(format!("{:>8.4}", s));
                                ui.end_row();
                            }
                        });
                });

                ui.separator();
                ui.label("Recent trades (most recent last):");
                egui::Grid::new("trades_grid")
                    .striped(true)
                    .show(ui, |ui| {
                        ui.label("Time");
                        ui.label("Side");
                        ui.label("Size");
                        ui.end_row();
                        for tr in &snap.trades {
                            ui.label(format_ts(self.time_mode, tr.ts));
                            ui.label(&tr.side);
                            ui.label(&tr.size_str);
                            ui.end_row();
                        }
                    });
            });
    }

    fn ui_trading_panel(&self, ui: &mut egui::Ui) {
        ui.heading("Trading Panel (UI only)");
        ui.label("This panel is currently UI-only (no live orders).");
        ui.separator();
        ui.label(format!("Ticker: {}", self.current_ticker));
        ui.label("You can wire this to your own backend later.");
        ui.separator();

        ui.horizontal(|ui| {
            ui.label("Size (units):");
            ui.add(egui::DragValue::new(&mut 0.01_f64).speed(0.001));
        });
        ui.horizontal(|ui| {
            ui.label("Leverage:");
            ui.add(egui::DragValue::new(&mut 5_i32).clamp_range(1..=50));
        });
        ui.horizontal(|ui| {
            ui.label("Order type:");
            ui.radio(true, "Market");
            ui.radio(false, "Limit");
        });
        ui.horizontal(|ui| {
            if ui.button("BUY (stub)").clicked() {
                // no-op
            }
            if ui.button("SELL (stub)").clicked() {
                // no-op
            }
        });
    }

    fn ui_script_engine(&mut self, ui: &mut egui::Ui, snap_opt: Option<&Snapshot>) {
        ui.heading("Script Engine (Rhai)");
        ui.label("Return a map like #{ tf: 60, history: 300, auto_y: true, signal: \"none\" }");
        ui.separator();

        let height = ui.available_height().max(200.0);
        let width = ui.available_width();

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .max_height(height * 0.6)
            .show(ui, |ui| {
                ui.add_sized(
                    [width, height * 0.6],
                    egui::TextEdit::multiline(&mut self.script_source)
                        .font(egui::TextStyle::Monospace)
                        .code_editor()
                        .desired_rows(16),
                );
            });

        ui.horizontal(|ui| {
            if ui.button("Run script").clicked() {
                self.run_script_engine(snap_opt);
            }
            if ui.button("Reset to example").clicked() {
                self.script_source.clear();
                self.script_source.push_str(
                    "// Example bot script (Rhai)\n\
                     // Available: mid, best_bid, best_ask, tf, history\n\
                     // Return a map: #{ tf: 60, history: 300, auto_y: true, signal: \"none\", size: 0.0 }\n\
                     \n\
                     let out = #{};\n\
                     \n\
                     if mid > 0.0 {\n\
                         out.tf = 60;\n\
                         out.history = 300;\n\
                         out.auto_y = true;\n\
                     }\n\
                     \n\
                     if best_ask - best_bid > mid * 0.001 {\n\
                         out.signal = \"bubble\";\n\
                         out.size = 0.01;\n\
                         out.comment = \"Spread wider than 0.1%\";\n\
                     }\n\
                     \n\
                     out;\n",
                );
            }
        });

        ui.separator();

        if let Some(err) = &self.script_last_error {
            ui.colored_label(Color32::RED, format!("Error: {err}"));
        } else if !self.script_last_info.is_empty() {
            ui.colored_label(Color32::LIGHT_GREEN, &self.script_last_info);
        }

        if let Some(sig) = &self.bot_signal {
            ui.separator();
            ui.colored_label(
                Color32::YELLOW,
                format!("Bot signal: {sig} | size: {:.4}", self.bot_size),
            );
            if !self.bot_comment.is_empty() {
                ui.label(format!("Comment: {}", self.bot_comment));
            }
        }
    }

    fn ui_candles(&mut self, ui: &mut egui::Ui, snap_opt: Option<&Snapshot>, is_live: bool) {
        let snap = match snap_opt {
            Some(s) => s,
            None => {
                ui.label("No snapshot.");
                return;
            }
        };
        let series = self.series_from_snap(snap);
        if series.is_empty() {
            ui.label("No candles for current TF.");
            return;
        }

        let len = series.len();
        let window_len = self.chart.show_candles.min(len).max(1);
        let visible = &series[len - window_len..];

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

        let avail_h = ui.available_height().max(150.0);
        let avail_w = ui.available_width();
        let tf = self.chart.tf as f64;
        let last = visible.last().unwrap();
        let x_center = last.t as f64 + tf * 0.5;
        let base_span = tf * self.chart.show_candles as f64;
        let span = base_span / self.chart.x_zoom.max(1e-6);
        let x_min = x_center - span * 0.5 + self.chart.x_pan_secs;
        let x_max = x_center + span * 0.5 + self.chart.x_pan_secs;

        let mode = self.time_mode;
        let plot_resp = Plot::new(if is_live {
            "candles_live"
        } else {
            "candles_replay"
        })
        .height(avail_h * 0.98)
        .include_y(y_min)
        .include_y(y_max)
        .allow_drag(true)
        .allow_zoom(true)
        .x_axis_formatter(move |mark, _bounds, _| {
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

            let now_x = if is_live {
                self.live_ts as f64
            } else {
                self.replay_ts as f64
            };
            plot_ui.vline(VLine::new(now_x).name("now_ts"));
        });

        // vertical zoom (Shift + scroll over candles)
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
            let half_span = (self.chart.y_max - self.chart.y_min).max(1e-6) * factor * 0.5;
            self.chart.y_min = center - half_span;
            self.chart.y_max = center + half_span;
        }
    }

    fn ui_volume(&mut self, ui: &mut egui::Ui, snap_opt: Option<&Snapshot>, is_live: bool) {
        let snap = match snap_opt {
            Some(s) => s,
            None => {
                ui.label("No snapshot.");
                return;
            }
        };
        let series = self.series_from_snap(snap);
        if series.is_empty() {
            ui.label("No candles for current TF (so no volume).");
            return;
        }

        let len = series.len();
        let window_len = self.chart.show_candles.min(len).max(1);
        let visible = &series[len - window_len..];

        let avail_h = ui.available_height().max(120.0);
        let avail_w = ui.available_width();

        let tf = self.chart.tf as f64;
        let last = visible.last().unwrap();
        let x_center = last.t as f64 + tf * 0.5;
        let base_span = tf * self.chart.show_candles as f64;
        let span = base_span / self.chart.x_zoom.max(1e-6);
        let x_min = x_center - span * 0.5 + self.chart.x_pan_secs;
        let x_max = x_center + span * 0.5 + self.chart.x_pan_secs;

        let mode = self.time_mode;
        let plot_resp = Plot::new(if is_live {
            "volume_live_only"
        } else {
            "volume_replay_only"
        })
        .height(avail_h * 0.98)
        .include_y(0.0)
        .allow_drag(true)
        .allow_zoom(true)
        .x_axis_formatter(move |mark, _bounds, _| {
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

                let line_pts: PlotPoints = vec![[mid, 0.0], [mid, c.volume]].into();
                plot_ui.line(Line::new(line_pts).color(color).width(2.0));
            }
        });

        // vertical zoom (Shift + scroll)
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
            let half_span = (self.chart.y_max - self.chart.y_min).max(1e-6) * factor * 0.5;
            self.chart.y_min = center - half_span;
            self.chart.y_max = center + half_span;
        }
    }

    fn cell_index(row: usize, col: usize) -> usize {
        row * COLS + col
    }

    fn ui_grid(&mut self, ui: &mut egui::Ui) {
        // clone snapshots once so we don't fight the borrow checker
        let live_snap = self.live_snapshot.clone();
        let replay_snap = self.replay_snapshot.clone();
        let current_snap = match self.mode {
            Mode::Live => live_snap.as_ref(),
            Mode::Replay => replay_snap.as_ref(),
        };

        for row in 0..ROWS {
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(format!("Row {row} layout:"));
                // layout selector
                egui::ComboBox::from_id_source(format!("row_layout_{row}"))
                    .selected_text(self.row_layouts[row].label())
                    .show_ui(ui, |ui| {
                        for layout in [
                            RowLayout::Three,
                            RowLayout::Two12,
                            RowLayout::Two23,
                            RowLayout::One123,
                        ] {
                            if ui
                                .selectable_label(self.row_layouts[row] == layout, layout.label())
                                .clicked()
                            {
                                self.row_layouts[row] = layout;
                            }
                        }
                    });

                // ratio for 2-cell layouts
                match self.row_layouts[row] {
                    RowLayout::Two12 | RowLayout::Two23 => {
                        ui.label("Big/small ratio:");
                        ui.add(
                            egui::Slider::new(&mut self.row_ratios[row], 0.2..=0.8)
                                .text("big"),
                        );
                    }
                    _ => {}
                }
            });

            let row_height = 220.0_f32; // baseline, each cell will scroll if needed
            let total_width = ui.available_width();

            ui.horizontal(|ui| {
                match self.row_layouts[row] {
                    RowLayout::Three => {
                        let cell_w = total_width / 3.0;
                        for col in 0..3 {
                            let idx = ComboApp::cell_index(row, col);
                            ui.vertical(|ui| {
                                ui.set_width(cell_w);
                                ui.set_min_height(row_height);
                                self.ui_cell(ui, row, col, idx, current_snap);
                            });
                        }
                    }
                    RowLayout::Two12 => {
                        let big_w = total_width * self.row_ratios[row];
                        let small_w = total_width - big_w;
                        // big cell uses col0+col1 -> logical idx row*3
                        let big_idx = ComboApp::cell_index(row, 0);
                        let small_idx = ComboApp::cell_index(row, 2);

                        ui.vertical(|ui| {
                            ui.set_width(big_w);
                            ui.set_min_height(row_height);
                            self.ui_cell(ui, row, 0, big_idx, current_snap);
                        });
                        ui.vertical(|ui| {
                            ui.set_width(small_w);
                            ui.set_min_height(row_height);
                            self.ui_cell(ui, row, 2, small_idx, current_snap);
                        });
                    }
                    RowLayout::Two23 => {
                        let big_w = total_width * self.row_ratios[row];
                        let small_w = total_width - big_w;
                        // small cell = col0 -> idx row*3
                        // big cell = col1+col2 -> idx row*3+1
                        let small_idx = ComboApp::cell_index(row, 0);
                        let big_idx = ComboApp::cell_index(row, 1);

                        ui.vertical(|ui| {
                            ui.set_width(small_w);
                            ui.set_min_height(row_height);
                            self.ui_cell(ui, row, 0, small_idx, current_snap);
                        });
                        ui.vertical(|ui| {
                            ui.set_width(big_w);
                            ui.set_min_height(row_height);
                            self.ui_cell(ui, row, 2, big_idx, current_snap);
                        });
                    }
                    RowLayout::One123 => {
                        let idx = ComboApp::cell_index(row, 0);
                        ui.vertical(|ui| {
                            ui.set_width(total_width);
                            ui.set_min_height(row_height * 1.2);
                            self.ui_cell(ui, row, 0, idx, current_snap);
                        });
                    }
                }
            });
        }
    }

    fn ui_cell(
        &mut self,
        ui: &mut egui::Ui,
        row: usize,
        col: usize,
        idx: usize,
        snap_opt: Option<&Snapshot>,
    ) {
        ui.group(|ui| {
            ui.horizontal(|ui| {
                ui.label(format!("Cell ({row},{col})"));
                egui::ComboBox::from_id_source(format!("cell_kind_{idx}"))
                    .selected_text(self.cell_kinds[idx].label())
                    .show_ui(ui, |ui| {
                        for k in CellKind::all() {
                            if ui
                                .selectable_label(self.cell_kinds[idx] == *k, k.label())
                                .clicked()
                            {
                                self.cell_kinds[idx] = *k;
                            }
                        }
                    });
            });

            ui.separator();

            match self.cell_kinds[idx] {
                CellKind::Empty => {
                    ui.label("Empty cell.");
                }
                CellKind::Summary => self.ui_summary(ui, snap_opt),
                CellKind::Candles => {
                    let is_live = matches!(self.mode, Mode::Live);
                    self.ui_candles(ui, snap_opt, is_live);
                }
                CellKind::Volume => {
                    let is_live = matches!(self.mode, Mode::Live);
                    self.ui_volume(ui, snap_opt, is_live);
                }
                CellKind::Depth => self.ui_depth(ui, snap_opt),
                CellKind::LaddersTrades => self.ui_ladders_trades(ui, snap_opt),
                CellKind::ScriptEngine => self.ui_script_engine(ui, snap_opt),
                CellKind::TradingPanel => self.ui_trading_panel(ui),
            }
        });
    }
}

impl eframe::App for ComboApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // For live mode, periodically refresh from CSV (simple approach).
        if matches!(self.mode, Mode::Live) {
            if let Some(td) = self.current_td_clone() {
                let latest = td.max_ts;
                if latest > self.live_ts {
                    self.live_ts = latest;
                    self.live_snapshot = Some(compute_snapshot_for(&td, self.live_ts));
                    self.ticker_data.insert(self.current_ticker.clone(), td);
                }
            }
        }

        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            self.ui_top_bar(ui);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    self.ui_grid(ui);
                });
        });

        ctx.request_repaint_after(Duration::from_millis(100));
    }
}

// ------------ main ------------

fn main() {
    let base_dir = PathBuf::from("data");

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size(egui::vec2(1400.0, 900.0)),
        ..Default::default()
    };

    let app_creator = move |_cc: &eframe::CreationContext<'_>| {
        Box::new(ComboApp::new(base_dir.clone())) as Box<dyn eframe::App>
    };

    if let Err(e) =
        eframe::run_native("dYdX CSV Viewer + Script Engine", native_options, Box::new(app_creator))
    {
        eprintln!("eframe error: {e}");
    }
}
