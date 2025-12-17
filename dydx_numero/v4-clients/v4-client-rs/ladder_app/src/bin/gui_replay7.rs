// ladder_app/src/bin/gui_replay_compat.rs
//
// Replay GUI compatible with gui_app28 CSV logs.
//
// Reads:
//   data/orderbook_ethusd.csv
//   data/orderbook_btcusd.csv
//   data/orderbook_solusd.csv
//   data/trades.csv
//
// Provides:
//   - Ticker selector (ETH-USD / BTC-USD / SOL-USD)
//   - Global time slider + play/pause + speed
//   - Candles + Volume + RSI (per ticker, built from orderbook midprice)
//   - Current orderbook snapshot (top 15 bids/asks)
//   - Recent trades table for current ticker
//   - Unix / Local time toggle
//   - Theme selector (5 palettes)
//
// Run (no mnemonic needed; offline replay):
//   cargo run -p ladder_app --bin gui_replay_compat

mod candle_agg;

use candle_agg::{Candle, CandleAgg};

use eframe::egui;
use egui::{Color32, Stroke};
use egui_plot::{GridMark, HLine, Line, Plot, PlotBounds, PlotPoints, Polygon, VLine};

use chrono::{Local, TimeZone};

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::time::Duration;

// ---- time display mode ----

#[derive(Clone, Copy, PartialEq, Eq)]
enum TimeDisplayMode {
    Unix,
    Local,
}

fn format_ts_common(mode: TimeDisplayMode, ts: u64) -> String {
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

// ---- themes (same structure as live GUI) ----

#[derive(Clone, Copy, PartialEq, Eq)]
enum ThemeKind {
    ClassicDark,
    NeonDark,
    LightClean,
    Solarized,
    Monochrome,
}

impl ThemeKind {
    fn label(&self) -> &'static str {
        match self {
            ThemeKind::ClassicDark => "Classic Dark",
            ThemeKind::NeonDark => "Neon Dark",
            ThemeKind::LightClean => "Light Clean",
            ThemeKind::Solarized => "Solarized-ish",
            ThemeKind::Monochrome => "Monochrome",
        }
    }

    fn all() -> &'static [ThemeKind] {
        &[
            ThemeKind::ClassicDark,
            ThemeKind::NeonDark,
            ThemeKind::LightClean,
            ThemeKind::Solarized,
            ThemeKind::Monochrome,
        ]
    }
}

#[derive(Clone, Copy)]
struct ThemePalette {
    dark: bool,
    up: Color32,
    down: Color32,
    depth_bid: Color32,
    depth_ask: Color32,
    volume_up: Color32,
    volume_down: Color32,
    rsi_line: Color32,
    text: Color32,
    window_bg: Color32,
    panel_bg: Color32,
    accent: Color32,
}

fn theme_palette(kind: ThemeKind) -> ThemePalette {
    match kind {
        ThemeKind::ClassicDark => ThemePalette {
            dark: true,
            up: Color32::from_rgb(0, 200, 0),
            down: Color32::from_rgb(220, 50, 47),
            depth_bid: Color32::from_rgb(80, 180, 80),
            depth_ask: Color32::from_rgb(220, 120, 80),
            volume_up: Color32::from_rgb(60, 160, 60),
            volume_down: Color32::from_rgb(180, 60, 60),
            rsi_line: Color32::from_rgb(130, 200, 255),
            text: Color32::from_rgb(230, 230, 230),
            window_bg: Color32::from_rgb(20, 20, 25),
            panel_bg: Color32::from_rgb(26, 28, 34),
            accent: Color32::from_rgb(130, 170, 255),
        },
        ThemeKind::NeonDark => ThemePalette {
            dark: true,
            up: Color32::from_rgb(0, 255, 180),
            down: Color32::from_rgb(255, 80, 120),
            depth_bid: Color32::from_rgb(0, 200, 140),
            depth_ask: Color32::from_rgb(255, 150, 80),
            volume_up: Color32::from_rgb(0, 180, 180),
            volume_down: Color32::from_rgb(255, 140, 160),
            rsi_line: Color32::from_rgb(180, 180, 255),
            text: Color32::from_rgb(230, 230, 255),
            window_bg: Color32::from_rgb(10, 10, 25),
            panel_bg: Color32::from_rgb(18, 22, 40),
            accent: Color32::from_rgb(120, 240, 255),
        },
        ThemeKind::LightClean => ThemePalette {
            dark: false,
            up: Color32::from_rgb(0, 150, 0),
            down: Color32::from_rgb(200, 60, 60),
            depth_bid: Color32::from_rgb(60, 160, 80),
            depth_ask: Color32::from_rgb(200, 120, 80),
            volume_up: Color32::from_rgb(60, 140, 60),
            volume_down: Color32::from_rgb(200, 80, 80),
            rsi_line: Color32::from_rgb(40, 90, 160),
            text: Color32::from_rgb(40, 40, 40),
            window_bg: Color32::from_rgb(240, 242, 245),
            panel_bg: Color32::from_rgb(252, 252, 255),
            accent: Color32::from_rgb(70, 120, 220),
        },
        ThemeKind::Solarized => ThemePalette {
            dark: true,
            up: Color32::from_rgb(133, 153, 0),
            down: Color32::from_rgb(220, 50, 47),
            depth_bid: Color32::from_rgb(88, 110, 117),
            depth_ask: Color32::from_rgb(203, 75, 22),
            volume_up: Color32::from_rgb(133, 153, 0),
            volume_down: Color32::from_rgb(211, 54, 130),
            rsi_line: Color32::from_rgb(38, 139, 210),
            text: Color32::from_rgb(253, 246, 227),
            window_bg: Color32::from_rgb(0, 43, 54),
            panel_bg: Color32::from_rgb(7, 54, 66),
            accent: Color32::from_rgb(181, 137, 0),
        },
        ThemeKind::Monochrome => ThemePalette {
            dark: true,
            up: Color32::from_rgb(180, 180, 180),
            down: Color32::from_rgb(80, 80, 80),
            depth_bid: Color32::from_rgb(200, 200, 200),
            depth_ask: Color32::from_rgb(120, 120, 120),
            volume_up: Color32::from_rgb(190, 190, 190),
            volume_down: Color32::from_rgb(110, 110, 110),
            rsi_line: Color32::from_rgb(210, 210, 210),
            text: Color32::from_rgb(230, 230, 230),
            window_bg: Color32::from_rgb(15, 15, 15),
            panel_bg: Color32::from_rgb(25, 25, 25),
            accent: Color32::from_rgb(200, 200, 200),
        },
    }
}

// ---- chart settings ----

#[derive(Clone)]
struct ChartSettings {
    y_min: f64,
    y_max: f64,
    show_candles: usize,
    auto_scale: bool,
}

// ---- Candles RSI helper ----

fn compute_rsi(closes: &[f64], period: usize) -> Vec<(f64, f64)> {
    if closes.len() < period + 1 {
        return Vec::new();
    }
    let mut out = Vec::new();

    for i in period..closes.len() {
        let window = &closes[i - period..=i];
        let mut gains = 0.0;
        let mut losses = 0.0;

        for w in 1..window.len() {
            let diff = window[w] - window[w - 1];
            if diff >= 0.0 {
                gains += diff;
            } else {
                losses -= diff;
            }
        }

        let avg_gain = gains / period as f64;
        let avg_loss = losses / period as f64;
        let rsi = if avg_loss == 0.0 {
            100.0
        } else {
            let rs = avg_gain / avg_loss;
            100.0 - (100.0 / (1.0 + rs))
        };

        out.push((i as f64, rsi));
    }

    out
}

// ---- book CSV types ----

#[derive(Clone, Copy, Debug)]
enum BookMsgType {
    Initial,
    Update,
}

#[derive(Clone, Copy, Debug)]
enum BookSide {
    Bid,
    Ask,
}

#[derive(Clone, Debug)]
struct BookCsvEvent {
    ts: u64,
    msg_type: BookMsgType,
    side: BookSide,
    price: f64,
    size: f64,
}

// ---- trade CSV type ----

#[derive(Clone, Debug)]
struct TradeCsvEvent {
    ts: u64,
    ticker: String,
    side: String,
    size: f64,
    tx_hash: String,
}

// ---- Ordered price key for BTreeMap ----

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
struct PriceKey(f64);

impl Eq for PriceKey {}

impl Ord for PriceKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0
            .partial_cmp(&other.0)
            .unwrap_or(Ordering::Equal)
    }
}

// ---- ReplayBook ----

#[derive(Default, Clone, Debug)]
struct ReplayBook {
    bids: BTreeMap<PriceKey, f64>,
    asks: BTreeMap<PriceKey, f64>,
}

impl ReplayBook {
    fn apply(&mut self, ev: &BookCsvEvent) {
        let map = match ev.side {
            BookSide::Bid => &mut self.bids,
            BookSide::Ask => &mut self.asks,
        };
        let key = PriceKey(ev.price);
        if ev.size == 0.0 {
            map.remove(&key);
        } else {
            map.insert(key, ev.size);
        }
    }

    fn best_bid_ask(&self) -> (Option<(f64, f64)>, Option<(f64, f64)>) {
        let bid = self
            .bids
            .iter()
            .next_back()
            .map(|(k, v)| (k.0, *v));
        let ask = self
            .asks
            .iter()
            .next()
            .map(|(k, v)| (k.0, *v));
        (bid, ask)
    }
}

// ---- per-ticker replay state ----

struct TickerReplayState {
    book_events: Vec<BookCsvEvent>,
    trade_events: Vec<TradeCsvEvent>,
    book: ReplayBook,

    tf_30s: CandleAgg,
    tf_1m: CandleAgg,
    tf_3m: CandleAgg,
    tf_5m: CandleAgg,

    book_idx: usize,
    trade_idx: usize,
    trades_window: Vec<TradeCsvEvent>,
}

impl TickerReplayState {
    fn new(book_events: Vec<BookCsvEvent>, trade_events: Vec<TradeCsvEvent>) -> Self {
        Self {
            book_events,
            trade_events,
            book: ReplayBook::default(),
            tf_30s: CandleAgg::new(30),
            tf_1m: CandleAgg::new(60),
            tf_3m: CandleAgg::new(180),
            tf_5m: CandleAgg::new(300),
            book_idx: 0,
            trade_idx: 0,
            trades_window: Vec::new(),
        }
    }

    fn reset(&mut self) {
        self.book = ReplayBook::default();
        self.tf_30s = CandleAgg::new(30);
        self.tf_1m = CandleAgg::new(60);
        self.tf_3m = CandleAgg::new(180);
        self.tf_5m = CandleAgg::new(300);
        self.book_idx = 0;
        self.trade_idx = 0;
        self.trades_window.clear();
    }

    fn min_ts(&self) -> u64 {
        self.book_events
            .first()
            .map(|e| e.ts)
            .or_else(|| self.trade_events.first().map(|t| t.ts))
            .unwrap_or(0)
    }

    fn max_ts(&self) -> u64 {
        let last_book = self.book_events.last().map(|e| e.ts).unwrap_or(0);
        let last_trade = self.trade_events.last().map(|t| t.ts).unwrap_or(0);
        last_book.max(last_trade)
    }

    fn current_series_for_tf(&self, tf: u64, cutoff: u64) -> Vec<Candle> {
        let all = match tf {
            30 => self.tf_30s.get_series(),
            60 => self.tf_1m.get_series(),
            180 => self.tf_3m.get_series(),
            300 => self.tf_5m.get_series(),
            _ => self.tf_1m.get_series(),
        };
        all.into_iter().filter(|c| c.t <= cutoff).collect()
    }

    fn apply_until(&mut self, target_ts: u64) {
        // book events
        while self.book_idx < self.book_events.len()
            && self.book_events[self.book_idx].ts <= target_ts
        {
            let ev = &self.book_events[self.book_idx];
            self.book.apply(ev);

            let (bid, ask) = self.book.best_bid_ask();
            if let (Some((bp, bs)), Some((ap, asz))) = (bid, ask) {
                let mid = (bp + ap) * 0.5;
                let vol = bs.abs() + asz.abs();
                let vol = if vol <= 0.0 { 1.0 } else { vol };
                self.tf_30s.update(ev.ts, mid, vol);
                self.tf_1m.update(ev.ts, mid, vol);
                self.tf_3m.update(ev.ts, mid, vol);
                self.tf_5m.update(ev.ts, mid, vol);
            }

            self.book_idx += 1;
        }

        // trade events
        while self.trade_idx < self.trade_events.len()
            && self.trade_events[self.trade_idx].ts <= target_ts
        {
            let ev = self.trade_events[self.trade_idx].clone();
            self.trades_window.push(ev);
            if self.trades_window.len() > 100 {
                self.trades_window.remove(0);
            }
            self.trade_idx += 1;
        }
    }
}

// ---- Loader for CSVs ----

fn parse_book_file(path: &str) -> Vec<BookCsvEvent> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => {
            eprintln!("replay: cannot open {}", path);
            return Vec::new();
        }
    };
    let reader = BufReader::new(file);
    let mut out = Vec::new();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with("ts,") {
            continue; // skip header
        }
        let parts: Vec<&str> = line.split(',').collect();
        if parts.len() != 5 {
            continue;
        }
        let ts: u64 = match parts[0].parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let msg_type = match parts[1] {
            "initial" => BookMsgType::Initial,
            "update" => BookMsgType::Update,
            _ => continue,
        };
        let side = match parts[2] {
            "bid" => BookSide::Bid,
            "ask" => BookSide::Ask,
            _ => continue,
        };
        let price: f64 = match parts[3].parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let size: f64 = match parts[4].parse() {
            Ok(v) => v,
            Err(_) => continue,
        };

        out.push(BookCsvEvent {
            ts,
            msg_type,
            side,
            price,
            size,
        });
    }

    out.sort_by_key(|e| e.ts);
    out
}

fn parse_trades_file(path: &str) -> Vec<TradeCsvEvent> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => {
            eprintln!("replay: cannot open {}", path);
            return Vec::new();
        }
    };
    let reader = BufReader::new(file);
    let mut out = Vec::new();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with("ts,") {
            continue; // skip header lines (may be repeated)
        }
        let parts: Vec<&str> = line.split(',').collect();
        if parts.len() != 5 {
            continue;
        }
        let ts: u64 = match parts[0].parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let tx_hash = parts[1].to_string();
        let ticker = parts[2].to_string();
        let side = parts[3].to_string();
        let size: f64 = match parts[4].parse() {
            Ok(v) => v,
            Err(_) => continue,
        };

        out.push(TradeCsvEvent {
            ts,
            ticker,
            side,
            size,
            tx_hash,
        });
    }

    out.sort_by_key(|t| t.ts);
    out
}

struct AllReplayData {
    tickers: Vec<String>,
    states: HashMap<String, TickerReplayState>,
    min_ts: u64,
    max_ts: u64,
}

fn load_replay_data() -> AllReplayData {
    let tickers = vec![
        "ETH-USD".to_string(),
        "BTC-USD".to_string(),
        "SOL-USD".to_string(),
    ];

    let paths = vec![
        ("ETH-USD", "data/orderbook_ethusd.csv"),
        ("BTC-USD", "data/orderbook_btcusd.csv"),
        ("SOL-USD", "data/orderbook_solusd.csv"),
    ];

    let trade_events_all = parse_trades_file("data/trades.csv");

    let mut states = HashMap::new();
    let mut global_min_ts = u64::MAX;
    let mut global_max_ts = 0_u64;

    for (ticker, path) in paths {
        let book_events = parse_book_file(path);
        let trades_for_ticker: Vec<TradeCsvEvent> = trade_events_all
            .iter()
            .filter(|t| t.ticker == ticker)
            .cloned()
            .collect();

        let state = TickerReplayState::new(book_events, trades_for_ticker);

        let min_ts = state.min_ts();
        let max_ts = state.max_ts();
        if min_ts > 0 {
            global_min_ts = global_min_ts.min(min_ts);
        }
        global_max_ts = global_max_ts.max(max_ts);

        states.insert(ticker.to_string(), state);
    }

    if global_min_ts == u64::MAX {
        global_min_ts = 0;
    }

    AllReplayData {
        tickers,
        states,
        min_ts: global_min_ts,
        max_ts: global_max_ts,
    }
}

// ---- ReplayApp ----

struct ReplayApp {
    tickers: Vec<String>,
    states: HashMap<String, TickerReplayState>,
    current_ticker: String,

    time_mode: TimeDisplayMode,
    current_theme: ThemeKind,

    selected_tf: u64,
    chart: ChartSettings,
    candles_bounds: Option<PlotBounds>,

    current_ts: u64,
    min_ts: u64,
    max_ts: u64,
    is_playing: bool,
    playback_speed: f64, // seconds per step
}

impl ReplayApp {
    fn new(data: AllReplayData) -> Self {
        let ticker = data
            .tickers
            .first()
            .cloned()
            .unwrap_or_else(|| "ETH-USD".to_string());

        let mut app = Self {
            tickers: data.tickers,
            states: data.states,
            current_ticker: ticker,
            time_mode: TimeDisplayMode::Local,
            current_theme: ThemeKind::ClassicDark,
            selected_tf: 60,
            chart: ChartSettings {
                y_min: 2950.0,
                y_max: 3050.0,
                show_candles: 160,
                auto_scale: true,
            },
            candles_bounds: None,
            current_ts: data.min_ts,
            min_ts: data.min_ts,
            max_ts: data.max_ts,
            is_playing: false,
            playback_speed: 1.0,
        };

        // Prime initial state for first ticker
        if let Some(state) = app.states.get_mut(&app.current_ticker) {
            state.reset();
            state.apply_until(app.current_ts);
        }

        app
    }

    fn apply_theme(&self, ctx: &egui::Context) {
        let pal = theme_palette(self.current_theme);
        let mut style = (*ctx.style()).clone();

        style.visuals = if pal.dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };

        style.visuals.override_text_color = Some(pal.text);
        style.visuals.window_fill = pal.window_bg;
        style.visuals.panel_fill = pal.panel_bg;
        style.visuals.hyperlink_color = pal.accent;
        style.visuals.selection.bg_fill = pal.accent;
        style.visuals.selection.stroke.color = pal.text;

        ctx.set_style(style);
    }

    fn current_palette(&self) -> ThemePalette {
        theme_palette(self.current_theme)
    }

    fn format_ts(&self, ts: u64) -> String {
        format_ts_common(self.time_mode, ts)
    }

    fn current_state(&self) -> Option<&TickerReplayState> {
        self.states.get(&self.current_ticker)
    }

    fn current_state_mut(&mut self) -> Option<&mut TickerReplayState> {
        self.states.get_mut(&self.current_ticker)
    }

    fn switch_tf(&mut self, new_tf: u64) {
        self.selected_tf = new_tf;
        self.candles_bounds = None;
    }

    fn switch_ticker(&mut self, new_ticker: &str) {
        if self.current_ticker == new_ticker {
            return;
        }
        self.current_ticker = new_ticker.to_string();
        self.candles_bounds = None;
        if let Some(state) = self.current_state_mut() {
            state.reset();
            state.apply_until(self.current_ts);
        }
    }

    fn step_playback(&mut self) {
        if !self.is_playing {
            return;
        }
        if self.max_ts <= self.min_ts {
            return;
        }

        let step = self.playback_speed.max(1.0) as u64;
        let new_ts = (self.current_ts + step).min(self.max_ts);
        self.advance_to(new_ts);
        if self.current_ts >= self.max_ts {
            self.is_playing = false;
        }
    }

    fn advance_to(&mut self, target_ts: u64) {
        if target_ts < self.min_ts {
            return;
        }
        self.current_ts = target_ts.min(self.max_ts);

        if let Some(state) = self.current_state_mut() {
            // naive approach: if target < current, recompute from start
            // for simplicity, always recompute; easier for correctness
            state.reset();
            state.apply_until(self.current_ts);
        }
    }

    // ---- UI ----

    fn ui_top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Replay GUI (offline)");

            ui.separator();
            ui.label("Ticker:");
            egui::ComboBox::from_id_source("replay_ticker_combo")
                .selected_text(&self.current_ticker)
                .show_ui(ui, |ui| {
                    for t in &self.tickers {
                        if ui
                            .selectable_label(&self.current_ticker == t, t)
                            .clicked()
                        {
                            self.switch_ticker(t);
                        }
                    }
                });

            ui.separator();
            ui.label("TF:");
            if ui.button("30s").clicked() {
                self.switch_tf(30);
            }
            if ui.button("1m").clicked() {
                self.switch_tf(60);
            }
            if ui.button("3m").clicked() {
                self.switch_tf(180);
            }
            if ui.button("5m").clicked() {
                self.switch_tf(300);
            }

            ui.separator();
            ui.checkbox(&mut self.chart.auto_scale, "Auto-scale Y");

            ui.separator();
            ui.label("Time:");
            ui.selectable_value(&mut self.time_mode, TimeDisplayMode::Unix, "Unix");
            ui.selectable_value(&mut self.time_mode, TimeDisplayMode::Local, "Local");

            ui.separator();
            ui.label("Theme:");
            let label = self.current_theme.label();
            egui::ComboBox::from_id_source("replay_theme_combo")
                .selected_text(label)
                .show_ui(ui, |ui| {
                    for theme in ThemeKind::all() {
                        ui.selectable_value(&mut self.current_theme, *theme, theme.label());
                    }
                });
        });

        ui.separator();

        // playback controls
        ui.horizontal(|ui| {
            if ui.button(if self.is_playing { "Pause" } else { "Play" }).clicked() {
                self.is_playing = !self.is_playing;
            }
            if ui.button("⏮ Reset").clicked() {
                self.current_ts = self.min_ts;
                if let Some(state) = self.current_state_mut() {
                    state.reset();
                    state.apply_until(self.current_ts);
                }
            }

            ui.label("Speed (secs/step):");
            ui.add(
                egui::Slider::new(&mut self.playback_speed, 1.0..=60.0)
                    .logarithmic(true),
            );

            ui.separator();
            ui.label(format!("Current t: {} ({})", self.current_ts, self.format_ts(self.current_ts)));
        });

        ui.separator();

        // time slider
        if self.max_ts > self.min_ts {
            let mut temp_ts = self.current_ts;
            let changed = ui
                .add(
                    egui::Slider::new(&mut temp_ts, self.min_ts..=self.max_ts)
                        .text("timeline"),
                )
                .changed();
            if changed {
                self.advance_to(temp_ts);
            }
        } else {
            ui.label("Not enough data to build a timeline.");
        }
    }

    fn ui_candles_and_panels(&mut self, ui: &mut egui::Ui) {
        let state = match self.current_state() {
            Some(s) => s,
            None => {
                ui.label("No state for current ticker.");
                return;
            }
        };

        let series_vec = state.current_series_for_tf(self.selected_tf, self.current_ts);
        if series_vec.is_empty() {
            ui.label("No candles yet for this ticker at current time.");
            return;
        }

        ui.horizontal(|ui| {
            ui.label("History (candles):");
            ui.add(
                egui::Slider::new(&mut self.chart.show_candles, 20..=600)
                    .logarithmic(true),
            );

            if !self.chart.auto_scale {
                ui.separator();
                ui.label("Manual Y:");
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
        });

        ui.separator();

        let len = series_vec.len();
        let window_len = self.chart.show_candles.min(len).max(1);
        let visible = &series_vec[len - window_len..];

        let (y_min, y_max) = if self.chart.auto_scale {
            let lo = visible.iter().map(|c| c.low).fold(f64::MAX, f64::min);
            let hi = visible.iter().map(|c| c.high).fold(f64::MIN, f64::max);
            let span = (hi - lo).max(1e-3);
            let pad = span * 0.05;
            (lo - pad, hi + pad)
        } else {
            (self.chart.y_min, self.chart.y_max)
        };

        if self.chart.auto_scale {
            self.chart.y_min = y_min;
            self.chart.y_max = y_max;
        }

        let avail_h = ui.available_height();
        let avail_w = ui.available_width();
        let candles_h = avail_h * 0.45;
        let volume_h = avail_h * 0.20;
        let rsi_h = avail_h * 0.20;
        let bottom_h = avail_h * 0.15;

        let tf = self.selected_tf as f64;
        let last = visible.last().unwrap();
        let pal = self.current_palette();

        // candles plot
        ui.allocate_ui(egui::vec2(avail_w, candles_h), |ui| {
            let mode = self.time_mode;
            let ctx = ui.ctx().clone();
            let prev_bounds = self.candles_bounds;

            let mut new_bounds_out: Option<PlotBounds> = None;

            Plot::new("replay_candles_plot")
                .height(candles_h)
                .include_y(y_min)
                .include_y(y_max)
                .allow_drag(true)
                .allow_zoom(true)
                .allow_scroll(true)
                .allow_boxed_zoom(true)
                .x_axis_formatter(move |mark: GridMark, _range, _transform| {
                    format_ts_common(mode, mark.value as u64)
                })
                .show(ui, |plot_ui| {
                    for c in visible {
                        let mid = c.t as f64 + tf * 0.5;
                        let body_half = tf * 0.35;
                        let body_left = mid - body_half;
                        let body_right = mid + body_half;

                        let top = c.open.max(c.close);
                        let bot = c.open.min(c.close);

                        let color = if c.close >= c.open { pal.up } else { pal.down };

                        let wick_pts: PlotPoints =
                            vec![[mid, c.low], [mid, c.high]].into();
                        plot_ui.line(Line::new(wick_pts).color(color));

                        let body_pts: PlotPoints = vec![
                            [body_left, bot],
                            [body_left, top],
                            [body_right, top],
                            [body_right, bot],
                        ]
                        .into();
                        plot_ui.polygon(
                            Polygon::new(body_pts)
                                .fill_color(color)
                                .stroke(Stroke::new(1.0, color)),
                        );
                    }

                    let now_x = last.t as f64 + tf;
                    let now_px = last.close;
                    plot_ui.hline(HLine::new(now_px).name("now_px"));
                    plot_ui.vline(VLine::new(now_x).name("now_t"));

                    // viewport logic for "current candle in the middle"
                    let mut bounds = plot_ui.plot_bounds();

                    if prev_bounds.is_none() {
                        let center_x = last.t as f64 + tf * 0.5;
                        let half_span =
                            (tf * self.chart.show_candles as f64).max(1.0) / 2.0;
                        let x_min = center_x - half_span;
                        let x_max = center_x + half_span;

                        bounds = PlotBounds::from_min_max(
                            [x_min, y_min],
                            [x_max, y_max],
                        );
                    }

                    if let Some(prev) = prev_bounds {
                        let mut restore_x = false;
                        let mut restore_y = false;

                        ctx.input(|i| {
                            if i.raw_scroll_delta.y != 0.0 {
                                if i.modifiers.shift {
                                    restore_x = true;
                                } else {
                                    restore_y = true;
                                }
                            }
                        });

                        if restore_y {
                            bounds = PlotBounds::from_min_max(
                                [bounds.min()[0], prev.min()[1]],
                                [bounds.max()[0], prev.max()[1]],
                            );
                        }

                        if restore_x {
                            bounds = PlotBounds::from_min_max(
                                [prev.min()[0], bounds.min()[1]],
                                [prev.max()[0], bounds.max()[1]],
                            );
                        }
                    }

                    plot_ui.set_plot_bounds(bounds);
                    new_bounds_out = Some(bounds);
                });

            self.candles_bounds = new_bounds_out;
        });

        ui.separator();

        // volume plot
        ui.allocate_ui(egui::vec2(avail_w, volume_h), |ui| {
            let max_vol = visible
                .iter()
                .map(|c| c.volume)
                .fold(0.0_f64, f64::max)
                .max(1.0);

            let mode = self.time_mode;
            let pal = self.current_palette();
            Plot::new("replay_volume_plot")
                .height(volume_h)
                .include_y(0.0)
                .include_y(max_vol)
                .allow_drag(true)
                .allow_zoom(true)
                .allow_scroll(true)
                .allow_boxed_zoom(true)
                .x_axis_formatter(move |mark: GridMark, _range, _transform| {
                    format_ts_common(mode, mark.value as u64)
                })
                .show(ui, |plot_ui| {
                    for c in visible {
                        let mid = c.t as f64 + tf * 0.5;
                        let v = c.volume;
                        let color = if c.close >= c.open {
                            pal.volume_up
                        } else {
                            pal.volume_down
                        };
                        let pts: PlotPoints = vec![[mid, 0.0], [mid, v]].into();
                        plot_ui.line(Line::new(pts).color(color).width(2.0));
                    }
                });
        });

        ui.separator();

        // RSI plot
        ui.allocate_ui(egui::vec2(avail_w, rsi_h), |ui| {
            let closes_all: Vec<f64> = series_vec.iter().map(|c| c.close).collect();
            let rsi_all = compute_rsi(&closes_all, 14);

            let start_idx = (len - window_len) as usize;
            let mut rsi_visible = Vec::new();
            for (idx_f, v) in rsi_all {
                let idx = idx_f as usize;
                if idx >= start_idx && idx < series_vec.len() {
                    let t = series_vec[idx].t as f64;
                    rsi_visible.push((t, v));
                }
            }

            let mode = self.time_mode;
            let pal = self.current_palette();
            Plot::new("replay_rsi_plot")
                .height(rsi_h)
                .include_y(0.0)
                .include_y(100.0)
                .allow_drag(true)
                .allow_zoom(true)
                .allow_scroll(true)
                .allow_boxed_zoom(true)
                .x_axis_formatter(move |mark: GridMark, _range, _transform| {
                    format_ts_common(mode, mark.value as u64)
                })
                .show(ui, |plot_ui| {
                    if !rsi_visible.is_empty() {
                        let pts: PlotPoints = rsi_visible
                            .iter()
                            .map(|(t, v)| [*t, *v])
                            .collect::<Vec<_>>()
                            .into();
                        plot_ui
                            .line(Line::new(pts).name("RSI").color(pal.rsi_line).width(2.0));
                        plot_ui.hline(HLine::new(70.0));
                        plot_ui.hline(HLine::new(30.0));
                    }
                });
        });

        ui.separator();

        // bottom: info + orderbook + trades
        ui.allocate_ui(egui::vec2(avail_w, bottom_h), |ui| {
            ui.columns(3, |cols| {
                // last candle info
                cols[0].group(|ui| {
                    ui.label(format!("Last candle ({})", self.current_ticker));
                    if let Some(c) = series_vec.last() {
                        ui.label(format!("t_start unix: {}", c.t));
                        ui.label(format!("t (display): {}", self.format_ts(c.t)));
                        ui.label(format!("O: {:.2}", c.open));
                        ui.label(format!("H: {:.2}", c.high));
                        ui.label(format!("L: {:.2}", c.low));
                        ui.label(format!("C: {:.2}", c.close));
                        ui.label(format!("V: {:.4}", c.volume));
                    }
                });

                // current orderbook snapshot
                cols[1].group(|ui| {
                    ui.label(format!("Orderbook snapshot ({})", self.current_ticker));
                    if let Some(st) = self.current_state() {
                        egui::Grid::new("replay_ob_grid")
                            .striped(true)
                            .show(ui, |ui| {
                                ui.label("Side");
                                ui.label("Price");
                                ui.label("Size");
                                ui.end_row();

                                // show top bids
                                let mut bids: Vec<(f64, f64)> = st
                                    .book
                                    .bids
                                    .iter()
                                    .rev()
                                    .take(8)
                                    .map(|(k, v)| (k.0, *v))
                                    .collect();
                                let mut asks: Vec<(f64, f64)> = st
                                    .book
                                    .asks
                                    .iter()
                                    .take(8)
                                    .map(|(k, v)| (k.0, *v))
                                    .collect();

                                // simple: fill bids first, then asks
                                for (p, s) in bids.drain(..) {
                                    ui.label("Bid");
                                    ui.label(format!("{:.2}", p));
                                    ui.label(format!("{:.6}", s));
                                    ui.end_row();
                                }
                                for (p, s) in asks.drain(..) {
                                    ui.label("Ask");
                                    ui.label(format!("{:.2}", p));
                                    ui.label(format!("{:.6}", s));
                                    ui.end_row();
                                }
                            });
                    } else {
                        ui.label("No book data.");
                    }
                });

                // recent trades
                cols[2].group(|ui| {
                    ui.label(format!("Recent trades ({})", self.current_ticker));
                    if let Some(st) = self.current_state() {
                        egui::ScrollArea::vertical()
                            .max_height(bottom_h * 0.9)
                            .show(ui, |ui| {
                                egui::Grid::new("replay_trades_grid")
                                    .striped(true)
                                    .show(ui, |ui| {
                                        ui.label("t");
                                        ui.label("side");
                                        ui.label("size");
                                        ui.end_row();

                                        for tr in st.trades_window.iter().rev().take(40) {
                                            ui.label(self.format_ts(tr.ts));
                                            ui.label(&tr.side);
                                            ui.label(format!("{:.6}", tr.size));
                                            ui.end_row();
                                        }
                                    });
                            });
                    } else {
                        ui.label("No trades yet.");
                    }
                });
            });
        });
    }
}

impl eframe::App for ReplayApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.apply_theme(ctx);

        // advance replay if playing
        self.step_playback();

        egui::TopBottomPanel::top("replay_top_panel").show(ctx, |ui| {
            self.ui_top_bar(ui);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    self.ui_candles_and_panels(ui);
                });
        });

        ctx.request_repaint_after(Duration::from_millis(33));
    }
}

fn main() {
    let data = load_replay_data();
    let options = eframe::NativeOptions::default();

    if let Err(e) = eframe::run_native(
        "Ladder GUI REPLAY (egui, compatible with gui_app28)",
        options,
        Box::new(|_cc| Box::new(ReplayApp::new(data))),
    ) {
        eprintln!("eframe error: {e}");
    }
}
