// ladder_app/src/bin/full_gui_x01.rs
//
// GUI that uses ONLY the CSVs written by data_daemon02 as its engine.
// No direct dYdX connections here.
//
// - Live mode:
//     * Periodically reloads ./data/orderbook_{TICKER}.csv and trades_{TICKER}.csv
//     * Builds full candle history from CSV up to latest timestamp
//     * Shows depth, ladders, candles, volume, recent trades
//
// - Replay mode:
//     * Same CSVs, but you choose a timestamp via slider
//     * Reconstructs the book + candles + recent trades at that time
//
// Shared:
//   - Ticker dropdown: ETH-USD / BTC-USD / SOL-USD
//   - Time display: Unix vs Local
//   - Timeframes: 1s → 1d
//   - Mouse drag/zoom, Shift+scroll vertical zoom
//   - Theme + layout toggles
//
// Run (with daemon already running):
//   cargo run -p ladder_app --bin full_gui_x01
//

use chrono::{Local, TimeZone};
use eframe::egui;
use eframe::egui::{Color32, RichText};
use egui_plot::{Line, Plot, PlotBounds, PlotPoints, VLine};

use std::cmp::{max, min};
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::time::{Duration, Instant};

// -------------------- basic helpers --------------------

fn now_unix() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// integer key for price ladder
type PriceKey = i64;

fn price_to_key(price: f64) -> PriceKey {
    (price * 10_000.0).round() as PriceKey
}

fn key_to_price(key: PriceKey) -> f64 {
    key as f64 / 10_000.0
}

// -------------------- candle struct --------------------

#[derive(Clone, Debug)]
struct Candle {
    pub t: u64,       // bucket start
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

// -------------------- time display --------------------

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

// -------------------- timeframes --------------------

// From 1 second up to 1 day
const TF_LIST: &[(u64, &str)] = &[
    (1, "1s"),
    (5, "5s"),
    (15, "15s"),
    (30, "30s"),
    (60, "1m"),
    (120, "2m"),
    (180, "3m"),
    (300, "5m"),
    (600, "10m"),
    (900, "15m"),
    (1800, "30m"),
    (3600, "1h"),
    (7200, "2h"),
    (14_400, "4h"),
    (28_800, "8h"),
    (86_400, "1d"),
];

// -------------------- chart + layout settings --------------------

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
            selected_tf: 60, // default 1m
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ThemeKind {
    Dark,
    Light,
    Ocean,
    Fire,
    Matrix,
}

impl ThemeKind {
    fn label(self) -> &'static str {
        match self {
            ThemeKind::Dark => "Dark",
            ThemeKind::Light => "Light",
            ThemeKind::Ocean => "Ocean",
            ThemeKind::Fire => "Fire",
            ThemeKind::Matrix => "Matrix",
        }
    }
}

#[derive(Clone)]
struct Theme {
    bg: Color32,
    text: Color32,
    candle_up: Color32,
    candle_down: Color32,
    volume: Color32,
    bid_line: Color32,
    ask_line: Color32,
}

impl Theme {
    fn from_kind(kind: ThemeKind) -> Self {
        match kind {
            ThemeKind::Dark => Self {
                bg: Color32::from_rgb(15, 15, 20),
                text: Color32::from_rgb(230, 230, 240),
                candle_up: Color32::from_rgb(80, 200, 120),
                candle_down: Color32::from_rgb(240, 80, 80),
                volume: Color32::from_rgb(120, 170, 240),
                bid_line: Color32::from_rgb(80, 200, 120),
                ask_line: Color32::from_rgb(240, 120, 80),
            },
            ThemeKind::Light => Self {
                bg: Color32::from_rgb(245, 245, 250),
                text: Color32::from_rgb(20, 20, 40),
                candle_up: Color32::from_rgb(0, 150, 0),
                candle_down: Color32::from_rgb(200, 40, 40),
                volume: Color32::from_rgb(60, 110, 200),
                bid_line: Color32::from_rgb(0, 140, 0),
                ask_line: Color32::from_rgb(180, 70, 40),
            },
            ThemeKind::Ocean => Self {
                bg: Color32::from_rgb(5, 18, 30),
                text: Color32::from_rgb(210, 230, 250),
                candle_up: Color32::from_rgb(60, 200, 200),
                candle_down: Color32::from_rgb(250, 120, 100),
                volume: Color32::from_rgb(90, 150, 240),
                bid_line: Color32::from_rgb(70, 200, 170),
                ask_line: Color32::from_rgb(240, 110, 150),
            },
            ThemeKind::Fire => Self {
                bg: Color32::from_rgb(20, 6, 6),
                text: Color32::from_rgb(255, 230, 210),
                candle_up: Color32::from_rgb(240, 200, 40),
                candle_down: Color32::from_rgb(255, 80, 60),
                volume: Color32::from_rgb(220, 120, 60),
                bid_line: Color32::from_rgb(255, 200, 80),
                ask_line: Color32::from_rgb(255, 120, 90),
            },
            ThemeKind::Matrix => Self {
                bg: Color32::from_rgb(2, 8, 2),
                text: Color32::from_rgb(150, 255, 150),
                candle_up: Color32::from_rgb(0, 255, 120),
                candle_down: Color32::from_rgb(0, 180, 80),
                volume: Color32::from_rgb(0, 200, 200),
                bid_line: Color32::from_rgb(0, 255, 0),
                ask_line: Color32::from_rgb(0, 200, 200),
            },
        }
    }
}

#[derive(Clone)]
struct LayoutSettings {
    show_depth: bool,
    show_ladders: bool,
    show_trades: bool,
    show_volume: bool,
    depth_height_frac: f32,
}

impl Default for LayoutSettings {
    fn default() -> Self {
        Self {
            show_depth: true,
            show_ladders: true,
            show_trades: true,
            show_volume: true,
            depth_height_frac: 0.35,
        }
    }
}

// -------------------- modes --------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Live,
    Replay,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReplayTab {
    Orderbook,
    Candles,
}

// -------------------- CSV + replay structures --------------------

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

#[derive(Clone, Debug)]
struct Snapshot {
    bids: BTreeMap<PriceKey, f64>,
    asks: BTreeMap<PriceKey, f64>,
    candles: HashMap<u64, Vec<Candle>>, // tf -> candles
    last_mid: f64,
    last_vol: f64,
    trades: Vec<TradeCsvEvent>,
}

// -------------------- CSV loading --------------------

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
        let ts: u64 = match parts[0].parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let tk = parts[1].trim_matches('"').to_string();
        let kind = parts[2].to_string();
        let side = parts[3].to_string();
        let price: f64 = match parts[4].parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let size: f64 = match parts[5].parse() {
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
        let ts: u64 = match parts[0].parse() {
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
    let mut max_ts = 0;

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

// -------------------- snapshot reconstruction --------------------
//
// Brute-force candles directly from CSV by binning each book event into
// OHLCV buckets across ALL TF_LIST timeframes.
//

fn compute_snapshot_for(data: &TickerData, target_ts: u64) -> Snapshot {
    let mut bids: BTreeMap<PriceKey, f64> = BTreeMap::new();
    let mut asks: BTreeMap<PriceKey, f64> = BTreeMap::new();

    // For each TF, maintain BTreeMap<bucket_start, Candle>
    let mut per_tf: HashMap<u64, BTreeMap<u64, Candle>> = HashMap::new();
    for (tf, _) in TF_LIST {
        per_tf.insert(*tf, BTreeMap::new());
    }

    for e in &data.book_events {
        if e.ts > target_ts {
            break;
        }

        // Update book
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

        // Only build candles when we have both sides for a mid
        if let (Some((bp, _)), Some((ap, _))) = (bids.iter().next_back(), asks.iter().next()) {
            let mid = (key_to_price(*bp) + key_to_price(*ap)) * 0.5;
            let vol = e.size.abs().max(0.0);

            for (tf, _) in TF_LIST {
                if let Some(m) = per_tf.get_mut(tf) {
                    let bucket = if *tf == 0 { e.ts } else { (e.ts / tf) * tf };
                    let c = m.entry(bucket).or_insert(Candle {
                        t: bucket,
                        open: mid,
                        high: mid,
                        low: mid,
                        close: mid,
                        volume: 0.0,
                    });
                    if mid > c.high {
                        c.high = mid;
                    }
                    if mid < c.low {
                        c.low = mid;
                    }
                    c.close = mid;
                    c.volume += vol;
                }
            }
        }
    }

    // Convert BTreeMap -> Vec<Candle> sorted by time
    let mut candles: HashMap<u64, Vec<Candle>> = HashMap::new();
    for (tf, _) in TF_LIST {
        if let Some(m) = per_tf.get(tf) {
            let mut v: Vec<Candle> = m.values().cloned().collect();
            v.sort_by_key(|c| c.t);
            candles.insert(*tf, v);
        }
    }

    // trades up to target_ts (last 200)
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

    // Pick last candle from 1m or, if empty, from smallest TF with data
    let (last_mid, last_vol) = if let Some(series) = candles.get(&60).and_then(|v| v.last()) {
        (series.close, series.volume)
    } else {
        let mut chosen: Option<&Candle> = None;
        for (_tf, v) in &candles {
            if v.is_empty() {
                continue;
            }
            chosen = v.last();
        }
        if let Some(c) = chosen {
            (c.close, c.volume)
        } else {
            (0.0, 0.0)
        }
    };

    Snapshot {
        bids,
        asks,
        candles,
        last_mid,
        last_vol,
        trades,
    }
}

// -------------------- main app --------------------

struct ComboApp {
    // mode + time
    mode: Mode,
    time_mode: TimeDisplayMode,

    // theme + layout
    theme_kind: ThemeKind,
    theme: Theme,
    layout: LayoutSettings,

    // chart
    chart: ChartSettings,

    // tickers
    base_dir: String,
    tickers: Vec<String>,
    current_ticker: String,

    // CSV-backed data from daemon
    replay_data: HashMap<String, TickerData>,
    last_reload: Instant,
    reload_secs: f32,

    // live view
    live_snapshot: Option<Snapshot>,
    live_ts: u64,

    // replay view
    replay_ts: u64,
    replay_tab: ReplayTab,
}

impl ComboApp {
    fn new(base_dir: String) -> Self {
        let tickers = vec![
            "ETH-USD".to_string(),
            "BTC-USD".to_string(),
            "SOL-USD".to_string(),
        ];
        let current_ticker = "ETH-USD".to_string();

        let mut replay_data = HashMap::new();
        for tk in &tickers {
            if let Some(td) = load_ticker_data(&base_dir, tk) {
                replay_data.insert(tk.clone(), td);
            }
        }

        let mut live_ts = 0u64;
        let mut live_snapshot = None;
        let mut replay_ts = 0u64;

        if let Some(td) = replay_data.get(&current_ticker) {
            live_ts = td.max_ts;
            if live_ts > 0 {
                live_snapshot = Some(compute_snapshot_for(td, live_ts));
            }
            replay_ts = td.max_ts;
        }

        Self {
            mode: Mode::Live,
            time_mode: TimeDisplayMode::Local,
            theme_kind: ThemeKind::Dark,
            theme: Theme::from_kind(ThemeKind::Dark),
            layout: LayoutSettings::default(),
            chart: ChartSettings::default(),
            base_dir,
            tickers,
            current_ticker,
            replay_data,
            last_reload: Instant::now(),
            reload_secs: 1.0,
            live_snapshot,
            live_ts,
            replay_ts,
            replay_tab: ReplayTab::Candles,
        }
    }

    fn current_ticker_data(&self) -> Option<&TickerData> {
        self.replay_data.get(&self.current_ticker)
    }

    fn maybe_reload_from_csv(&mut self) {
        let elapsed = self.last_reload.elapsed();
        if elapsed < Duration::from_secs_f32(self.reload_secs) {
            return;
        }
        self.last_reload = Instant::now();

        for tk in &self.tickers {
            if let Some(td) = load_ticker_data(&self.base_dir, tk) {
                self.replay_data.insert(tk.clone(), td);
            }
        }
    }

    fn tick_live(&mut self) {
        self.maybe_reload_from_csv();

        let td = match self.current_ticker_data().cloned() {
            Some(td) => td,
            None => {
                self.live_snapshot = None;
                return;
            }
        };

        if td.max_ts == 0 {
            self.live_snapshot = None;
            return;
        }

        if self.live_snapshot.is_none() || self.live_ts != td.max_ts {
            let max_ts = td.max_ts;
            self.live_ts = max_ts;
            self.live_snapshot = Some(compute_snapshot_for(&td, max_ts));
        }
    }

    fn ensure_replay_ts_in_range(&mut self) {
        if let Some(td) = self.current_ticker_data() {
            let min_ts = td.min_ts;
            let max_ts = td.max_ts;
            if min_ts == 0 && max_ts == 0 {
                return;
            }
            if self.replay_ts < min_ts {
                self.replay_ts = min_ts;
            }
            if self.replay_ts > max_ts {
                self.replay_ts = max_ts;
            }
        }
    }

    fn replay_series_for(&self, snap: &Snapshot) -> Vec<Candle> {
        self.series_for_tf(snap, self.chart.selected_tf)
    }

    fn series_for_tf(&self, snap: &Snapshot, tf: u64) -> Vec<Candle> {
        if let Some(v) = snap.candles.get(&tf) {
            if !v.is_empty() {
                return v.clone();
            }
        }

        // fallback: 1m, then any TF
        if let Some(v) = snap.candles.get(&60) {
            if !v.is_empty() {
                return v.clone();
            }
        }
        for (_tf, v) in &snap.candles {
            if !v.is_empty() {
                return v.clone();
            }
        }
        Vec::new()
    }

    // ---------------- top bar ----------------

    fn ui_top_bar(&mut self, ui: &mut egui::Ui) {
        // theme update
        self.theme = Theme::from_kind(self.theme_kind);
        {
            let visuals = if matches!(self.theme_kind, ThemeKind::Light) {
                egui::Visuals::light()
            } else {
                egui::Visuals::dark()
            };
            ui.ctx().set_visuals(visuals);
        }

        ui.horizontal(|ui| {
            // Mode
            ui.label(RichText::new("Mode:").color(self.theme.text));
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

            // Ticker dropdown
            let tickers = self.tickers.clone();
            ui.menu_button(
                RichText::new(format!("Ticker: {}", self.current_ticker))
                    .color(self.theme.text),
                |ui| {
                    for t in &tickers {
                        let selected = *t == self.current_ticker;
                        if ui.selectable_label(selected, t).clicked() {
                            // switch ticker
                            self.current_ticker = t.clone();

                            // reset chart scaling so we don't reuse BTC axis on SOL etc.
                            self.chart.auto_y = true;
                            self.chart.x_pan_secs = 0.0;
                            self.chart.x_zoom = 1.0;

                            // refresh snapshots / timestamps for this ticker
                            if let Some(td) = self.current_ticker_data().cloned() {
                                let max_ts = td.max_ts;
                                self.live_ts = max_ts;
                                self.replay_ts = max_ts;
                                if max_ts > 0 {
                                    self.live_snapshot =
                                        Some(compute_snapshot_for(&td, max_ts));
                                } else {
                                    self.live_snapshot = None;
                                }
                            } else {
                                self.live_ts = 0;
                                self.replay_ts = 0;
                                self.live_snapshot = None;
                            }

                            ui.close_menu();
                        }
                    }
                },
            );

            ui.separator();

            // Time display
            ui.label(RichText::new("Time:").color(self.theme.text));
            for mode in [TimeDisplayMode::Local, TimeDisplayMode::Unix] {
                if ui
                    .selectable_label(self.time_mode == mode, mode.label())
                    .clicked()
                {
                    self.time_mode = mode;
                }
            }

            // Replay info
            if let Some(td) = self.current_ticker_data() {
                ui.separator();
                ui.label(
                    RichText::new(format!(
                        "Range: {} → {}",
                        format_ts(self.time_mode, td.min_ts),
                        format_ts(self.time_mode, td.max_ts)
                    ))
                    .color(self.theme.text),
                );
            }

            // Live TS
            if matches!(self.mode, Mode::Live) && self.live_ts > 0 {
                ui.separator();
                ui.label(
                    RichText::new(format!(
                        "Live ts (from CSV): {}",
                        format_ts(self.time_mode, self.live_ts)
                    ))
                    .color(self.theme.text),
                );
            }
        });

        ui.separator();

        // Replay controls
        if matches!(self.mode, Mode::Replay) {
            if let Some(td) = self.current_ticker_data() {
                let mut ts = self.replay_ts;
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Replay time:").color(self.theme.text));
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
                    ui.label(
                        RichText::new(format_ts(self.time_mode, ts))
                            .color(self.theme.text),
                    );
                });
                self.replay_ts = ts;

                ui.horizontal(|ui| {
                    ui.label("Replay view:");
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
            } else {
                ui.label(
                    RichText::new("No replay CSV for this ticker.")
                        .color(self.theme.text),
                );
            }

            ui.separator();
        }

        // Shared chart controls
        ui.horizontal(|ui| {
            ui.label(RichText::new("History candles:").color(self.theme.text));
            ui.add(
                egui::Slider::new(&mut self.chart.show_candles, 20..=1200)
                    .logarithmic(true),
            );

            ui.separator();
            ui.label(RichText::new("X zoom:").color(self.theme.text));
            ui.add(
                egui::Slider::new(&mut self.chart.x_zoom, 0.25..=8.0)
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
            ui.label(RichText::new("TF:").color(self.theme.text));
            for (tf, label) in TF_LIST {
                if ui
                    .selectable_label(self.chart.selected_tf == *tf, *label)
                    .clicked()
                {
                    self.chart.selected_tf = *tf;
                }
            }
        });

        ui.horizontal(|ui| {
            ui.checkbox(&mut self.chart.auto_y, "Auto Y");
            if !self.chart.auto_y {
                ui.label("Y min:");
                ui.add(egui::DragValue::new(&mut self.chart.y_min).speed(1.0));
                ui.label("Y max:");
                ui.add(egui::DragValue::new(&mut self.chart.y_max).speed(1.0));
                if ui.button("Reset Y").clicked() {
                    self.chart.auto_y = true;
                }
            }

            ui.separator();
            ui.label("Layout:");
            ui.checkbox(&mut self.layout.show_depth, "Depth");
            ui.checkbox(&mut self.layout.show_ladders, "Ladders");
            ui.checkbox(&mut self.layout.show_trades, "Trades");
            ui.checkbox(&mut self.layout.show_volume, "Volume");

            ui.separator();
            ui.label("Theme:");
            for kind in [
                ThemeKind::Dark,
                ThemeKind::Light,
                ThemeKind::Ocean,
                ThemeKind::Fire,
                ThemeKind::Matrix,
            ] {
                if ui
                    .selectable_label(self.theme_kind == kind, kind.label())
                    .clicked()
                {
                    self.theme_kind = kind;
                }
            }
        });

        ui.separator();
    }

    // ---------------- live UI ----------------

    fn ui_live(&mut self, ui: &mut egui::Ui) {
        self.tick_live();

        ui.heading(
            RichText::new(format!("LIVE (from daemon CSVs) {}", self.current_ticker))
                .color(self.theme.text),
        );

        if self.live_snapshot.is_none() {
            ui.label(
                RichText::new(
                    "No CSV data yet. Wait for data_daemon02 to write some lines.",
                )
                .color(self.theme.text),
            );
            return;
        }

        let snap = self.live_snapshot.as_ref().unwrap().clone();
        let series_vec = self.series_for_tf(&snap, self.chart.selected_tf);

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let avail_w = ui.available_width();
                let avail_h = ui.available_height();
                let depth_h = if self.layout.show_depth {
                    avail_h * self.layout.depth_height_frac
                } else {
                    0.0
                };

                if self.layout.show_depth || self.layout.show_ladders || self.layout.show_trades {
                    ui.allocate_ui(egui::vec2(avail_w, depth_h.max(150.0)), |ui| {
                        ui.horizontal(|ui| {
                            if self.layout.show_depth {
                                let depth_w = avail_w * 0.45;
                                ui.allocate_ui(
                                    egui::vec2(depth_w, depth_h.max(150.0)),
                                    |ui| {
                                        self.ui_depth_plot(ui, &snap);
                                    },
                                );
                            }

                            if self.layout.show_ladders || self.layout.show_trades {
                                ui.separator();
                                let ladders_w = avail_w * 0.55;
                                ui.allocate_ui(
                                    egui::vec2(ladders_w, depth_h.max(150.0)),
                                    |ui| {
                                        self.ui_ladders_and_trades(ui, &snap);
                                    },
                                );
                            }
                        });
                    });

                    ui.separator();
                }

                self.ui_candles_generic(ui, &series_vec, Some(&snap), true);
            });
    }

    fn ui_depth_plot(&self, ui: &mut egui::Ui, snap: &Snapshot) {
        let avail_h = ui.available_height();

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

        Plot::new("live_depth")
            .height(avail_h * 0.95)
            .show(ui, |plot_ui| {
                if !bid_points.is_empty() {
                    let pts: PlotPoints = bid_points
                        .iter()
                        .map(|(x, y)| [*x, *y])
                        .collect::<Vec<_>>()
                        .into();
                    plot_ui.line(
                        Line::new(pts)
                            .name("Bids")
                            .color(self.theme.bid_line),
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
                            .name("Asks")
                            .color(self.theme.ask_line),
                    );
                }
            });
    }

    fn ui_ladders_and_trades(&self, ui: &mut egui::Ui, snap: &Snapshot) {
        ui.label(
            RichText::new("Snapshot ladders").color(self.theme.text),
        );

        ui.columns(2, |cols| {
            if self.layout.show_ladders {
                cols[0].label("Bids");
                egui::Grid::new("live_bids_grid")
                    .striped(true)
                    .show(&mut cols[0], |ui| {
                        ui.label("Price");
                        ui.label("Size");
                        ui.end_row();
                        for (k, s) in snap.bids.iter().rev().take(30) {
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
                        for (k, s) in snap.asks.iter().take(30) {
                            let p = key_to_price(*k);
                            ui.label(format!("{:>9.2}", p));
                            ui.label(format!("{:>8.4}", s));
                            ui.end_row();
                        }
                    });
            }
        });

        ui.separator();
        ui.label(
            RichText::new(format!(
                "Last mid: {:.4}   Last vol: {:.6}",
                snap.last_mid, snap.last_vol
            ))
            .color(self.theme.text),
        );

        if self.layout.show_trades {
            ui.separator();
            ui.label(RichText::new("Recent trades:").color(self.theme.text));
            egui::ScrollArea::vertical()
                .max_height(180.0)
                .show(ui, |ui| {
                    egui::Grid::new("live_trades_grid")
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
    }

    // ---------------- replay UI ----------------

    fn ui_replay(&mut self, ui: &mut egui::Ui) {
        self.maybe_reload_from_csv();
        self.ensure_replay_ts_in_range();

        let snapshot = self
            .current_ticker_data()
            .map(|td| compute_snapshot_for(td, self.replay_ts));

        if snapshot.is_none() {
            ui.heading(
                RichText::new("No replay data for this ticker.")
                    .color(self.theme.text),
            );
            ui.label(
                RichText::new("Ensure ./data/orderbook_*.csv exists.")
                    .color(self.theme.text),
            );
            return;
        }

        let snap = snapshot.unwrap();

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| match self.replay_tab {
                ReplayTab::Orderbook => self.ui_replay_orderbook(ui, &snap),
                ReplayTab::Candles => {
                    let series_vec = self.replay_series_for(&snap);
                    self.ui_candles_generic(ui, &series_vec, Some(&snap), false);
                }
            });
    }

    fn ui_replay_orderbook(&self, ui: &mut egui::Ui, snap: &Snapshot) {
        ui.heading(
            RichText::new(format!(
                "REPLAY {} @ {}",
                self.current_ticker,
                format_ts(self.time_mode, self.replay_ts)
            ))
            .color(self.theme.text),
        );

        let avail_w = ui.available_width();
        let avail_h = ui.available_height();
        let depth_w = avail_w * 0.45;
        let ladders_w = avail_w * 0.55;

        ui.horizontal(|ui| {
            if self.layout.show_depth {
                ui.allocate_ui(egui::vec2(depth_w, avail_h), |ui| {
                    self.ui_depth_plot(ui, snap);
                });
            }

            ui.separator();

            if self.layout.show_ladders || self.layout.show_trades {
                ui.allocate_ui(egui::vec2(ladders_w, avail_h), |ui| {
                    self.ui_ladders_and_trades(ui, snap);
                });
            }
        });
    }

    // ---------------- candles + volume ----------------

    fn ui_candles_generic(
        &mut self,
        ui: &mut egui::Ui,
        series_vec: &Vec<Candle>,
        _snap: Option<&Snapshot>,
        is_live: bool,
    ) {
        if series_vec.is_empty() {
            ui.label(
                RichText::new(if is_live {
                    "No candles yet; waiting for daemon CSVs to accumulate enough book events."
                } else {
                    "No candles at this replay time."
                })
                .color(self.theme.text),
            );
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
        let candles_h = if self.layout.show_volume {
            avail_h * 0.7
        } else {
            avail_h * 0.95
        };
        let volume_h = if self.layout.show_volume {
            avail_h * 0.3
        } else {
            0.0
        };

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
            let theme = self.theme.clone();
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

                    let color = if c.close >= c.open {
                        theme.candle_up
                    } else {
                        theme.candle_down
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
                    now_unix() as f64
                } else {
                    visible.last().map(|c| c.t as f64 + tf * 0.5).unwrap_or(0.0)
                };
                plot_ui.vline(VLine::new(now_x).name("cursor_ts"));
            });

            // vertical zoom: Shift + scroll
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

        ui.separator();

        // Volume
        if self.layout.show_volume {
            ui.allocate_ui(egui::vec2(avail_w, volume_h), |ui| {
                let mode = self.time_mode;
                let theme = self.theme.clone();
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
                        plot_ui.line(
                            Line::new(line_pts)
                                .color(theme.volume)
                                .width(2.0),
                        );
                    }
                });

                // vertical zoom also works on volume
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
            });
        }
    }
}

// -------------------- eframe::App impl --------------------

impl eframe::App for ComboApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            self.ui_top_bar(ui);
        });

        egui::CentralPanel::default().show(ctx, |ui| match self.mode {
            Mode::Live => self.ui_live(ui),
            Mode::Replay => self.ui_replay(ui),
        });

        ctx.request_repaint_after(Duration::from_millis(150));
    }
}

// -------------------- main --------------------

fn main() {
    let base_dir = "data".to_string();

    let options = eframe::NativeOptions::default();
    let app = ComboApp::new(base_dir);

    if let Err(e) = eframe::run_native(
        "dYdX CSV Viewer (daemon-powered)",
        options,
        Box::new(|_cc| Box::new(app)),
    ) {
        eprintln!("eframe error: {e}");
    }
}
