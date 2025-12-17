// ladder_app/src/bin/full_gui_x07.rs
//
// GUI that reads daemon-written CSVs and shows:
//   - orderbook depth + ladders
//   - recent trades
//   - candles + volume
//   - live (tail-follow) and replay (slider)
//   - Rhai script engine for layout + bot signals
//
// Data source (written by your daemon):
//   ./data/orderbook_{TICKER}.csv
//   ./data/trades_{TICKER}.csv
//
// Expected CSV format:
//   orderbook_*: ts,ticker,kind,side,price,size
//   trades_*:    ts,ticker,source,side,size_str
//
// Script engine:
//   - Script should end with a map literal, e.g.:
//
//     let signal = "";
//     let size = 0.0;
//
//     if mode == "live" && last_close > mid {
//         signal = "buy";
//         size = 0.01;
//     } else if mode == "live" && last_close < mid {
//         signal = "sell";
//         size = 0.01;
//     }
//
//     #{ tf: 60,
//        history: 200,
//        auto_y: true,
//        show_depth: true,
//        show_ladders: true,
//        show_trades: true,
//        show_volume: true,
//        reload_secs: 5.0,
//        theme: "Dark",
//        bot_signal: signal,
//        bot_size: size }
//
//   - Available script variables:
//       last_close: f64
//       last_volume: f64
//       mid: f64
//       mode: "live" | "replay"
//       ticker: string
//       bot_signal_prev: string
//       bot_size_prev: f64
//
//   - Recognized output keys in the map:
//       tf: i64                     // 30,60,180,300
//       history: i64                // number of candles
//       auto_y: bool
//       y_min: f64
//       y_max: f64
//       show_depth: bool
//       show_ladders: bool
//       show_trades: bool
//       show_volume: bool
//       reload_secs: f64
//       theme: "Dark" | "Light" | "Ocean" | "Matrix" | "Pastel"
//       bot_signal: String
//       bot_size: f64
//
// Run:
//   cargo run --release -p ladder_app --bin full_gui_x07
//

mod candle_agg;

use candle_agg::{Candle, CandleAgg};

use chrono::{Local, TimeZone};
use eframe::egui;
use egui::Color32;
use egui_plot::{Line, Plot, PlotBounds, PlotPoints, VLine};

use rhai::{Engine, EvalAltResult, Map as RhaiMap, Scope};

use std::cmp::{max, min};
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// ------------- basic helpers -------------

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_secs()
}

// integer keys for BTreeMap
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
    selected_tf: u64,
    auto_y: bool,
    y_min: f64,
    y_max: f64,
}

impl Default for ChartSettings {
    fn default() -> Self {
        Self {
            show_candles: 200,
            selected_tf: 60,
            auto_y: true,
            y_min: 0.0,
            y_max: 0.0,
        }
    }
}

#[derive(Clone)]
struct LayoutSettings {
    show_depth: bool,
    show_ladders: bool,
    show_trades: bool,
    show_volume: bool,
}

impl Default for LayoutSettings {
    fn default() -> Self {
        Self {
            show_depth: true,
            show_ladders: true,
            show_trades: true,
            show_volume: true,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ThemeChoice {
    Dark,
    Light,
    Ocean,
    Matrix,
    Pastel,
}

impl ThemeChoice {
    fn label(self) -> &'static str {
        match self {
            ThemeChoice::Dark => "Dark",
            ThemeChoice::Light => "Light",
            ThemeChoice::Ocean => "Ocean",
            ThemeChoice::Matrix => "Matrix",
            ThemeChoice::Pastel => "Pastel",
        }
    }
}

impl FromStr for ThemeChoice {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "dark" => Ok(ThemeChoice::Dark),
            "light" => Ok(ThemeChoice::Light),
            "ocean" => Ok(ThemeChoice::Ocean),
            "matrix" => Ok(ThemeChoice::Matrix),
            "pastel" => Ok(ThemeChoice::Pastel),
            _ => Err(()),
        }
    }
}

fn theme_colors(theme: ThemeChoice) -> (Color32, Color32, Color32) {
    match theme {
        ThemeChoice::Dark => (
            Color32::from_rgb(150, 220, 150), // up
            Color32::from_rgb(220, 120, 120), // down
            Color32::from_rgb(120, 170, 240), // volume
        ),
        ThemeChoice::Light => (
            Color32::from_rgb(0, 140, 0),
            Color32::from_rgb(180, 0, 0),
            Color32::from_rgb(50, 90, 180),
        ),
        ThemeChoice::Ocean => (
            Color32::from_rgb(80, 200, 180),
            Color32::from_rgb(40, 100, 180),
            Color32::from_rgb(40, 160, 220),
        ),
        ThemeChoice::Matrix => (
            Color32::from_rgb(0, 255, 0),
            Color32::from_rgb(0, 120, 0),
            Color32::from_rgb(0, 200, 120),
        ),
        ThemeChoice::Pastel => (
            Color32::from_rgb(200, 160, 220),
            Color32::from_rgb(240, 140, 160),
            Color32::from_rgb(200, 200, 120),
        ),
    }
}

// ------------- modes -------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Live,
    Replay,
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

// --- CSV loading ---

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
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
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
    let f = match File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let reader = BufReader::new(f);
    let mut out = Vec::new();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
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

// reconstruct snapshot at target_ts
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

// ------------- script helpers -------------

fn map_get_int(m: &RhaiMap, key: &str) -> Option<i64> {
    m.get(key).and_then(|v| {
        if v.is_int() {
            v.as_int().ok()
        } else {
            None
        }
    })
}

fn map_get_bool(m: &RhaiMap, key: &str) -> Option<bool> {
    m.get(key).and_then(|v| {
        if v.is_bool() {
            v.as_bool().ok()
        } else {
            None
        }
    })
}

fn map_get_float(m: &RhaiMap, key: &str) -> Option<f64> {
    m.get(key).and_then(|v| {
        if v.is_float() {
            v.as_float().ok()
        } else if v.is_int() {
            v.as_int().ok().map(|i| i as f64)
        } else {
            None
        }
    })
}

fn map_get_string(m: &RhaiMap, key: &str) -> Option<String> {
    m.get(key).map(|v| v.to_string())
}

// ------------- main app -------------

struct ComboApp {
    // mode + time
    mode: Mode,
    time_mode: TimeDisplayMode,

    // chart + layout + theme
    chart: ChartSettings,
    layout: LayoutSettings,
    theme: ThemeChoice,

    // data
    base_dir: String,
    tickers: Vec<String>,
    current_ticker: String,
    data: HashMap<String, TickerData>,
    last_reload: SystemTime,
    reload_interval_secs: f64,
    live_ts: u64,
    replay_ts: u64,

    // script engine
    engine: Engine,
    script_text: String,
    script_auto_run: bool,
    last_script_error: String,
    bot_last_signal: String,
    bot_last_size: f64,
}

impl ComboApp {
    fn new(base_dir: &str) -> Self {
        let tickers = vec![
            "ETH-USD".to_string(),
            "BTC-USD".to_string(),
            "SOL-USD".to_string(),
        ];

        let mut data = HashMap::new();
        let mut live_ts = now_unix();
        let mut replay_ts = now_unix();

        for tk in &tickers {
            if let Some(td) = load_ticker_data(base_dir, tk) {
                live_ts = td.max_ts;
                replay_ts = td.max_ts;
                data.insert(tk.clone(), td);
            }
        }

        let mut engine = Engine::new();
        engine.set_max_modules(0); // keep it lightweight

        let script_text = r#"
// Example script for layout + bot signal
//
// Inputs:
//   last_close, last_volume, mid, mode, ticker,
//   bot_signal_prev, bot_size_prev
//
// Return a map #{ ... } with any of:
//
//   tf: 30|60|180|300
//   history: number of candles
//   auto_y: bool
//   y_min, y_max: floats
//   show_depth, show_ladders, show_trades, show_volume: bool
//   reload_secs: float
//   theme: "Dark" | "Light" | "Ocean" | "Matrix" | "Pastel"
//   bot_signal: string
//   bot_size: float

let signal = "";
let size = 0.0;

if mode == "live" && last_close > mid {
    signal = "buy";
    size = 0.01;
} else if mode == "live" && last_close < mid {
    signal = "sell";
    size = 0.01;
}

#{ tf: 60,
   history: 200,
   auto_y: true,
   show_depth: true,
   show_ladders: true,
   show_trades: true,
   show_volume: true,
   reload_secs: 5.0,
   theme: "Dark",
   bot_signal: signal,
   bot_size: size }
"#.to_string();

        Self {
            mode: Mode::Live,
            time_mode: TimeDisplayMode::Local,
            chart: ChartSettings::default(),
            layout: LayoutSettings::default(),
            theme: ThemeChoice::Dark,

            base_dir: base_dir.to_string(),
            tickers,
            current_ticker: "ETH-USD".to_string(),
            data,
            last_reload: SystemTime::now(),
            reload_interval_secs: 5.0,
            live_ts,
            replay_ts,

            engine,
            script_text,
            script_auto_run: false,
            last_script_error: String::new(),
            bot_last_signal: String::new(),
            bot_last_size: 0.0,
        }
    }

    fn current_ticker_data(&self) -> Option<&TickerData> {
        self.data.get(&self.current_ticker)
    }

    fn maybe_reload_csvs(&mut self) {
        let now = SystemTime::now();
        if let Ok(elapsed) = now.duration_since(self.last_reload) {
            if elapsed.as_secs_f64() < self.reload_interval_secs {
                return;
            }
        }

        self.last_reload = now;

        for tk in &self.tickers {
            if let Some(td) = load_ticker_data(&self.base_dir, tk) {
                self.data.insert(tk.clone(), td);
            }
        }

        // keep live_ts/replay_ts within new ranges while avoiding borrow issues
        if let Some((min_ts, max_ts)) = self
            .current_ticker_data()
            .map(|td| (td.min_ts, td.max_ts))
        {
            if max_ts > 0 {
                if matches!(self.mode, Mode::Live) {
                    self.live_ts = max_ts;
                }
                if self.replay_ts < min_ts || self.replay_ts > max_ts {
                    self.replay_ts = max_ts;
                }
            }
        }
    }

    fn current_ts(&self) -> Option<u64> {
        self.current_ticker_data().map(|td| match self.mode {
            Mode::Live => {
                if self.live_ts == 0 {
                    td.max_ts
                } else {
                    self.live_ts.min(td.max_ts).max(td.min_ts)
                }
            }
            Mode::Replay => {
                if self.replay_ts == 0 {
                    td.max_ts
                } else {
                    self.replay_ts.min(td.max_ts).max(td.min_ts)
                }
            }
        })
    }

    fn series_for_snap<'a>(&self, snap: &'a Snapshot) -> &'a Vec<Candle> {
        match self.chart.selected_tf {
            30 => &snap.tf_30s,
            60 => &snap.tf_1m,
            180 => &snap.tf_3m,
            300 => &snap.tf_5m,
            _ => &snap.tf_1m,
        }
    }

    fn run_script(&mut self, snap: &Snapshot) {
        // compute last_close / last_volume in a block so the borrow of &self ends
        let (last_close, last_volume) = {
            let series = self.series_for_snap(snap);
            let lc = series.last().map(|c| c.close).unwrap_or(0.0);
            let lv = series.last().map(|c| c.volume).unwrap_or(0.0);
            (lc, lv)
        };

        let mut scope = Scope::new();
        scope.push("last_close", last_close);
        scope.push("last_volume", last_volume);
        scope.push("mid", snap.last_mid);
        scope.push(
            "mode",
            match self.mode {
                Mode::Live => "live".to_string(),
                Mode::Replay => "replay".to_string(),
            },
        );
        scope.push("ticker", self.current_ticker.clone());
        scope.push("bot_signal_prev", self.bot_last_signal.clone());
        scope.push("bot_size_prev", self.bot_last_size);

        let result: Result<RhaiMap, Box<EvalAltResult>> =
            self.engine.eval_with_scope(&mut scope, &self.script_text);

        match result {
            Ok(map) => {
                // apply TF (clamped to our supported ones)
                if let Some(tf_val) = map_get_int(&map, "tf") {
                    let tf = match tf_val {
                        x if x <= 30 => 30,
                        x if x <= 60 => 60,
                        x if x <= 180 => 180,
                        _ => 300,
                    };
                    self.chart.selected_tf = tf as u64;
                }

                // history
                if let Some(history) = map_get_int(&map, "history") {
                    self.chart.show_candles = history.max(10) as usize;
                }

                // Y settings
                if let Some(auto_y) = map_get_bool(&map, "auto_y") {
                    self.chart.auto_y = auto_y;
                }
                if let Some(y_min) = map_get_float(&map, "y_min") {
                    self.chart.y_min = y_min;
                }
                if let Some(y_max) = map_get_float(&map, "y_max") {
                    self.chart.y_max = y_max;
                }

                // layout flags
                if let Some(show_depth) = map_get_bool(&map, "show_depth") {
                    self.layout.show_depth = show_depth;
                }
                if let Some(show_ladders) = map_get_bool(&map, "show_ladders") {
                    self.layout.show_ladders = show_ladders;
                }
                if let Some(show_trades) = map_get_bool(&map, "show_trades") {
                    self.layout.show_trades = show_trades;
                }
                if let Some(show_volume) = map_get_bool(&map, "show_volume") {
                    self.layout.show_volume = show_volume;
                }

                // reload frequency
                if let Some(reload) = map_get_float(&map, "reload_secs") {
                    self.reload_interval_secs = reload.clamp(1.0, 300.0);
                }

                // theme
                if let Some(theme_str) = map_get_string(&map, "theme") {
                    if let Ok(th) = ThemeChoice::from_str(&theme_str) {
                        self.theme = th;
                    }
                }

                // bot outputs
                if let Some(sig) = map_get_string(&map, "bot_signal") {
                    self.bot_last_signal = sig;
                }
                if let Some(size) = map_get_float(&map, "bot_size") {
                    self.bot_last_size = size;
                }

                self.last_script_error.clear();
            }
            Err(e) => {
                self.last_script_error = e.to_string();
            }
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

            // ticker
            let tickers = self.tickers.clone();
            ui.menu_button(format!("Ticker: {}", self.current_ticker), |ui| {
                for t in &tickers {
                    let selected = *t == self.current_ticker;
                    if ui.selectable_label(selected, t).clicked() {
                        self.current_ticker = t.clone();
                        if let Some(td) = self.current_ticker_data() {
                            self.live_ts = td.max_ts;
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

            if let Some(td) = self.current_ticker_data() {
                ui.separator();
                ui.label(format!(
                    "Range: {} → {}",
                    format_ts(self.time_mode, td.min_ts),
                    format_ts(self.time_mode, td.max_ts)
                ));

                let ts_now = self.current_ts().unwrap_or(td.max_ts);
                ui.separator();
                ui.label(format!("Current ts: {}", format_ts(self.time_mode, ts_now)));
            } else {
                ui.separator();
                ui.label("No CSV data for this ticker yet.");
            }
        });

        ui.separator();

        // replay slider or tail-follow
        if matches!(self.mode, Mode::Replay) {
            if let Some(td) = self.current_ticker_data() {
                let mut ts = self.replay_ts.clamp(td.min_ts, td.max_ts);
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
                ui.label("Replay: no data.");
            }
            ui.separator();
        } else if let Some(td) = self.current_ticker_data() {
            // live: always track tail
            self.live_ts = td.max_ts;
        }

        // chart + layout controls
        ui.horizontal(|ui| {
            ui.label("History candles:");
            ui.add(
                egui::Slider::new(&mut self.chart.show_candles, 20..=1000)
                    .logarithmic(true),
            );

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
            }

            ui.separator();
            ui.checkbox(&mut self.layout.show_depth, "Depth");
            ui.checkbox(&mut self.layout.show_ladders, "Ladders");
            ui.checkbox(&mut self.layout.show_trades, "Trades");
            ui.checkbox(&mut self.layout.show_volume, "Volume");

            ui.separator();
            ui.label("Theme:");
            for th in [
                ThemeChoice::Dark,
                ThemeChoice::Light,
                ThemeChoice::Ocean,
                ThemeChoice::Matrix,
                ThemeChoice::Pastel,
            ] {
                if ui
                    .selectable_label(self.theme == th, th.label())
                    .clicked()
                {
                    self.theme = th;
                }
            }

            ui.separator();
            ui.label("Reload (s):");
            ui.add(
                egui::DragValue::new(&mut self.reload_interval_secs)
                    .speed(0.5)
                    .clamp_range(1.0..=300.0),
            );
        });

        ui.separator();

        // script auto-run toggle
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.script_auto_run, "Auto-run script");
            if ui.button("Run script now").clicked() {
                if let (Some(td), Some(ts)) = (self.current_ticker_data(), self.current_ts())
                {
                    let snap = compute_snapshot_for(td, ts);
                    self.run_script(&snap);
                }
            }

            if !self.bot_last_signal.is_empty() {
                ui.separator();
                ui.label(format!(
                    "Bot signal: {} size {:.6}",
                    self.bot_last_signal, self.bot_last_size
                ));
            }
        });

        if !self.last_script_error.is_empty() {
            ui.colored_label(Color32::RED, format!("Script error: {}", self.last_script_error));
        }

        ui.separator();
    }

    fn ui_depth_plot(&self, ui: &mut egui::Ui, snap: &Snapshot, height: f32) {
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

        let (up_color, down_color, _) = theme_colors(self.theme);

        Plot::new("depth_plot")
            .height(height)
            .allow_drag(true)
            .allow_zoom(true)
            .show(ui, |plot_ui| {
                if !bid_points.is_empty() {
                    let pts: PlotPoints = bid_points
                        .iter()
                        .map(|(x, y)| [*x, *y])
                        .collect::<Vec<_>>()
                        .into();
                    plot_ui.line(Line::new(pts).color(up_color).name("Bids"));
                }
                if !ask_points.is_empty() {
                    let pts: PlotPoints = ask_points
                        .iter()
                        .map(|(x, y)| [*x, *y])
                        .collect::<Vec<_>>()
                        .into();
                    plot_ui.line(Line::new(pts).color(down_color).name("Asks"));
                }
            });
    }

    fn ui_ladders_and_trades(&self, ui: &mut egui::Ui, snap: &Snapshot, height: f32) {
        ui.columns(2, |cols| {
            // ladders
            if self.layout.show_ladders {
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

                cols[0].separator();

                cols[0].label("Asks");
                egui::Grid::new("asks_grid")
                    .striped(true)
                    .show(&mut cols[0], |ui| {
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
            }

            // trades
            if self.layout.show_trades {
                cols[1].label("Recent trades:");
                egui::ScrollArea::vertical()
                    .max_height(height * 0.9)
                    .show(&mut cols[1], |ui| {
                        egui::Grid::new("trades_grid")
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
            }
        });
    }

    fn ui_candles_and_volume(
        &mut self,
        ui: &mut egui::Ui,
        snap: &Snapshot,
        height: f32,
    ) {
        let series = self.series_for_snap(snap);
        if series.is_empty() {
            ui.label("No candles yet for this view.");
            return;
        }

        let len = series.len();
        let window_len = self.chart.show_candles.min(len).max(1);
        let visible = &series[len - window_len..];

        // y-range
        let (y_min, y_max) = if self.chart.auto_y {
            let lo = visible.iter().map(|c| c.low).fold(f64::MAX, f64::min);
            let hi = visible.iter().map(|c| c.high).fold(f64::MIN, f64::max);
            let span = (hi - lo).max(1e-3);
            let pad = span * 0.05;
            let y0 = lo - pad;
            let y1 = hi + pad;
            self.chart.y_min = y0;
            self.chart.y_max = y1;
            (y0, y1)
        } else {
            (self.chart.y_min, self.chart.y_max)
        };

        let tf = match self.chart.selected_tf {
            30 => 30.0,
            60 => 60.0,
            180 => 180.0,
            300 => 300.0,
            _ => 60.0,
        };

        let avail_w = ui.available_width();
        let candles_h = height * if self.layout.show_volume { 0.7 } else { 1.0 };
        let volume_h = if self.layout.show_volume {
            height * 0.3
        } else {
            0.0
        };

        let (up_color, down_color, vol_color) = theme_colors(self.theme);
        let mode = self.time_mode;

        // candles
        ui.allocate_ui(egui::vec2(avail_w, candles_h), |ui| {
            let plot_resp = Plot::new("candles_plot")
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
                    // x-bounds: focus last window_len candles
                    let first = visible.first().unwrap();
                    let last = visible.last().unwrap();
                    let x_min = first.t as f64;
                    let x_max = (last.t as f64) + tf;
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
                            up_color
                        } else {
                            down_color
                        };

                        // wick
                        let wick_pts: PlotPoints =
                            vec![[mid, c.low], [mid, c.high]].into();
                        plot_ui.line(Line::new(wick_pts).color(color));

                        // body
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

                    // vertical marker at current candle center
                    let now_x = visible
                        .last()
                        .map(|c| c.t as f64 + tf * 0.5)
                        .unwrap_or(0.0);
                    plot_ui.vline(VLine::new(now_x).name("current"));
                });

            // vertical zoom via Shift + scroll over candles
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

        if self.layout.show_volume {
            ui.separator();
        }

        // volume
        if self.layout.show_volume {
            ui.allocate_ui(egui::vec2(avail_w, volume_h), |ui| {
                let mode = self.time_mode;
                Plot::new("volume_plot")
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

                        let first = visible.first().unwrap();
                        let last = visible.last().unwrap();
                        let x_min = first.t as f64;
                        let x_max = (last.t as f64) + tf;

                        plot_ui.set_plot_bounds(PlotBounds::from_min_max(
                            [x_min, 0.0],
                            [x_max, y_max_v],
                        ));

                        for c in visible {
                            let left = c.t as f64;
                            let mid = left + tf * 0.5;

                            let line_pts: PlotPoints =
                                vec![[mid, 0.0], [mid, c.volume]].into();
                            plot_ui.line(
                                Line::new(line_pts).color(vol_color).width(2.0),
                            );
                        }
                    });
            });
        }
    }

    fn ui_script_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Script engine (Rhai)");

        ui.label("Edit script and return a map #{ ... } with settings + bot signal.");
        ui.add(
            egui::TextEdit::multiline(&mut self.script_text)
                .code_editor()
                .desired_rows(16)
                .desired_width(f32::INFINITY),
        );

        ui.horizontal(|ui| {
            ui.checkbox(&mut self.script_auto_run, "Auto-run each tick");
            if ui.button("Run script now").clicked() {
                if let (Some(td), Some(ts)) = (self.current_ticker_data(), self.current_ts())
                {
                    let snap = compute_snapshot_for(td, ts);
                    self.run_script(&snap);
                }
            }
        });

        if !self.last_script_error.is_empty() {
            ui.colored_label(
                Color32::RED,
                format!("Script error: {}", self.last_script_error),
            );
        }

        if !self.bot_last_signal.is_empty() {
            ui.label(format!(
                "Last bot signal: {}  size {:.6}",
                self.bot_last_signal, self.bot_last_size
            ));
        }
    }

    fn ui_main_view(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        let avail_w = ui.available_width();
        let avail_h = ui.available_height();

        let top_h = avail_h * 0.4;
        let bottom_h = avail_h * 0.6;

        ui.allocate_ui(egui::vec2(avail_w, top_h), |ui| {
            ui.horizontal(|ui| {
                if self.layout.show_depth {
                    let depth_w = if self.layout.show_ladders || self.layout.show_trades {
                        avail_w * 0.45
                    } else {
                        avail_w
                    };
                    ui.allocate_ui(egui::vec2(depth_w, top_h), |ui| {
                        self.ui_depth_plot(ui, snap, top_h * 0.9);
                    });
                }

                if self.layout.show_ladders || self.layout.show_trades {
                    let ladders_w = if self.layout.show_depth {
                        avail_w * 0.55
                    } else {
                        avail_w
                    };
                    ui.separator();
                    ui.allocate_ui(egui::vec2(ladders_w, top_h), |ui| {
                        self.ui_ladders_and_trades(ui, snap, top_h * 0.9);
                    });
                }
            });
        });

        ui.separator();

        ui.allocate_ui(egui::vec2(avail_w, bottom_h), |ui| {
            self.ui_candles_and_volume(ui, snap, bottom_h);
        });
    }
}

impl eframe::App for ComboApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // reload CSVs occasionally
        self.maybe_reload_csvs();

        // compute current snapshot
        let snapshot_opt = {
            let td_opt = self.current_ticker_data();
            let ts_opt = self.current_ts();
            if let (Some(td), Some(ts)) = (td_opt, ts_opt) {
                Some(compute_snapshot_for(td, ts))
            } else {
                None
            }
        };

        // run script if enabled
        if self.script_auto_run {
            if let Some(ref snap) = snapshot_opt {
                self.run_script(snap);
            }
        }

        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            self.ui_top_bar(ui);
        });

        egui::SidePanel::right("script_panel")
            .resizable(true)
            .default_width(380.0)
            .show(ctx, |ui| {
                self.ui_script_panel(ui);
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(ref snap) = snapshot_opt {
                ui.heading(format!(
                    "{} mode — {}",
                    match self.mode {
                        Mode::Live => "LIVE",
                        Mode::Replay => "REPLAY",
                    },
                    self.current_ticker
                ));
                ui.separator();
                self.ui_main_view(ui, snap);
            } else {
                ui.heading("No data yet");
                ui.label("Make sure the daemon is running and writing CSVs into ./data.");
            }
        });

        ctx.request_repaint_after(Duration::from_millis(100));
    }
}

// ------------- main -------------

fn main() {
    let base_dir = "data";

    let options = eframe::NativeOptions::default();
    let app = ComboApp::new(base_dir);

    if let Err(e) = eframe::run_native(
        "dYdX CSV Live + Replay + Script",
        options,
        Box::new(|_cc| Box::new(app)),
    ) {
        eprintln!("eframe error: {e}");
    }
}
