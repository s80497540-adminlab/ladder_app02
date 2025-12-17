// ladder_app/src/bin/gui_replay1.rs
//
// Offline REPLAY app.
// Replays orderbook + candles + "fills" from CSV files produced by gui_app27:
//
//   data/orderbook_ethusd.csv  (book events)
//   data/trades.csv            (real trades placed from gui_app27)
//
// Features:
//   - No network, no wallet, pure offline
//   - Reconstructs orderbook over time
//   - Candles + volume + RSI (same style as live app, filled bodies)
//   - Orderbook + depth view
//   - Time toggle: Unix vs Local
//   - Theme selector (5 palettes)
//   - Replay controls: Play/Pause, Speed slider, Restart
//   - Data tab with detailed views of current book + trades + events
//   - Adjustable windows for trades/events
//   - Snapshot export of current state to data/replay_snapshot_<ts>.txt
//
// Usage:
//   1. Run gui_app27 for a while to collect data in data/*.csv
//   2. Then run this:
//
//      cargo run -p ladder_app --bin gui_replay1
//
// NOTE: This only replays ETH-USD currently (matches orderbook_ethusd.csv).

mod candle_agg;

use candle_agg::{Candle, CandleAgg};

use eframe::egui;
use egui::{Color32, Stroke};
use egui_plot::{GridMark, HLine, Line, Plot, PlotBounds, PlotPoints, Polygon, VLine};

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use chrono::{Local, TimeZone};

use std::collections::BTreeMap;
use std::fs;
use std::fs::File;
use std::fmt::Write as FmtWrite;
use std::io::{BufRead, BufReader};
use std::time::{Duration, Instant};

// ---- price key quantization (for BTreeMap) ----
// Store prices as i64 with 1e-4 precision
type PriceKey = i64;
const PRICE_SCALE: i64 = 10_000;

fn price_to_key(price: f64) -> PriceKey {
    (price * PRICE_SCALE as f64).round() as i64
}
fn key_to_price(k: PriceKey) -> f64 {
    k as f64 / PRICE_SCALE as f64
}

// ---- orderbook + trades CSV types ----

#[derive(Debug, Clone)]
struct OrderbookCsvEvent {
    ts: u64,
    msg_type: String,
    side: String, // "bid" or "ask"
    price: f64,
    size: f64,
}

#[derive(Debug, Clone)]
struct TradeCsvEvent {
    ts: u64,
    ticker: String,
    side: String,
    size: f64,
}

// simple in-memory orderbook (price stored as quantized i64)
#[derive(Default, Clone, Debug)]
struct LiveBook {
    pub bids: BTreeMap<PriceKey, f64>, // price_key -> size
    pub asks: BTreeMap<PriceKey, f64>,
}

impl LiveBook {
    fn apply_level(&mut self, side: &str, price: f64, size: f64) {
        let key = price_to_key(price);
        let map = if side == "bid" {
            &mut self.bids
        } else {
            &mut self.asks
        };

        if size == 0.0 {
            map.remove(&key);
        } else {
            map.insert(key, size);
        }
    }

    fn best_bid_ask(&self) -> (Option<(f64, f64)>, Option<(f64, f64)>) {
        let bid = self
            .bids
            .iter()
            .next_back()
            .map(|(k, s)| (key_to_price(*k), *s));
        let ask = self
            .asks
            .iter()
            .next()
            .map(|(k, s)| (key_to_price(*k), *s));
        (bid, ask)
    }
}

// time display
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

// chart settings
#[derive(Clone)]
struct ChartSettings {
    y_min: f64,
    y_max: f64,
    show_candles: usize,
    auto_scale: bool,
}

// fake trading sim (same as live, but no real orders)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PositionSide {
    Flat,
    Long,
    Short,
}

impl PositionSide {
    fn label(&self) -> &'static str {
        match self {
            PositionSide::Flat => "FLAT",
            PositionSide::Long => "LONG",
            PositionSide::Short => "SHORT",
        }
    }
}

#[derive(Clone, Debug)]
struct TradingState {
    wallet_usdc: f64,
    margin: f64,
    deposit_amount: f64,
    withdraw_amount: f64,
    leverage: f64,
    position: f64,
    side: PositionSide,
    entry_price: Option<f64>,
    realized_pnl: f64,
    take_profit: Option<f64>,
    stop_loss: Option<f64>,
    maint_rate: f64,
    last_liq_price: Option<f64>,
    last_liq_time: Option<u64>,
    liquidated_flag: bool,
}

impl TradingState {
    fn new() -> Self {
        Self {
            wallet_usdc: 5_000.0,
            margin: 100.0,
            deposit_amount: 100.0,
            withdraw_amount: 100.0,
            leverage: 5.0,
            position: 0.0,
            side: PositionSide::Flat,
            entry_price: None,
            realized_pnl: 0.0,
            take_profit: None,
            stop_loss: None,
            maint_rate: 0.005,
            last_liq_price: None,
            last_liq_time: None,
            liquidated_flag: false,
        }
    }

    fn deposit_to_margin(&mut self, amount: f64) {
        if amount <= 0.0 {
            return;
        }
        let amt = amount.min(self.wallet_usdc);
        if amt <= 0.0 {
            return;
        }
        self.wallet_usdc -= amt;
        self.margin += amt;
    }

    fn withdraw_from_margin(&mut self, amount: f64) {
        if amount <= 0.0 {
            return;
        }
        let amt = amount.min(self.margin);
        if amt <= 0.0 {
            return;
        }
        self.margin -= amt;
        self.wallet_usdc += amt;
    }

    fn notional(&self) -> f64 {
        self.margin * self.leverage
    }

    fn max_position_units(&self, mark: f64) -> f64 {
        if mark <= 0.0 {
            return 0.0;
        }
        (self.margin * self.leverage / mark).max(0.0)
    }

    fn is_open(&self) -> bool {
        self.entry_price.is_some()
            && self.position > 0.0
            && !matches!(self.side, PositionSide::Flat)
    }

    fn unrealized_pnl(&self, mark: f64) -> f64 {
        if let Some(entry) = self.entry_price {
            match self.side {
                PositionSide::Long => (mark - entry) * self.position,
                PositionSide::Short => (entry - mark) * self.position,
                PositionSide::Flat => 0.0,
            }
        } else {
            0.0
        }
    }

    fn equity(&self, mark: f64) -> f64 {
        self.margin + self.realized_pnl + self.unrealized_pnl(mark)
    }

    fn maintenance_margin(&self, mark: f64) -> f64 {
        let notional = self.position * mark;
        notional * self.maint_rate
    }

    fn open_at(&mut self, mark: f64) {
        if self.is_open() || self.side == PositionSide::Flat {
            return;
        }
        if self.margin <= 0.0 || self.leverage <= 0.0 || mark <= 0.0 {
            return;
        }

        if self.position <= 0.0 {
            self.position = self.max_position_units(mark);
        } else {
            let maxu = self.max_position_units(mark);
            if self.position > maxu {
                self.position = maxu;
            }
        }

        self.entry_price = Some(mark);
        self.liquidated_flag = false;
    }

    fn close_at(&mut self, mark: f64) {
        if !self.is_open() {
            return;
        }

        let upnl = self.unrealized_pnl(mark);

        self.margin += upnl;
        self.realized_pnl += upnl;
        if self.margin < 0.0 {
            self.margin = 0.0;
        }

        self.position = 0.0;
        self.entry_price = None;
        self.side = PositionSide::Flat;
        self.take_profit = None;
        self.stop_loss = None;
    }

    fn liquidate_at(&mut self, mark: f64, ts: u64) {
        if !self.is_open() {
            return;
        }

        let upnl = self.unrealized_pnl(mark);

        self.margin += upnl;
        self.realized_pnl += upnl;

        self.margin = 0.0;

        self.position = 0.0;
        self.entry_price = None;
        self.side = PositionSide::Flat;
        self.take_profit = None;
        self.stop_loss = None;

        self.last_liq_price = Some(mark);
        self.last_liq_time = Some(ts);
        self.liquidated_flag = true;
    }

    fn bump_tp(&mut self, mark: f64, delta: f64) {
        let base = self.take_profit.unwrap_or(mark);
        self.take_profit = Some(base + delta);
    }

    fn bump_sl(&mut self, mark: f64, delta: f64) {
        let base = self.stop_loss.unwrap_or(mark);
        self.stop_loss = Some(base + delta);
    }

    fn check_tp_sl(&mut self, mark: f64) {
        if !self.is_open() {
            return;
        }
        let tp = self.take_profit;
        let sl = self.stop_loss;

        match self.side {
            PositionSide::Long => {
                if let Some(tp) = tp {
                    if mark >= tp {
                        self.close_at(mark);
                        return;
                    }
                }
                if let Some(sl) = sl {
                    if mark <= sl {
                        self.close_at(mark);
                        return;
                    }
                }
            }
            PositionSide::Short => {
                if let Some(tp) = tp {
                    if mark <= tp {
                        self.close_at(mark);
                        return;
                    }
                }
                if let Some(sl) = sl {
                    if mark >= sl {
                        self.close_at(mark);
                        return;
                    }
                }
            }
            PositionSide::Flat => {}
        }
    }

    fn check_liquidation(&mut self, mark: f64, ts: u64) {
        if !self.is_open() {
            return;
        }
        let equity = self.equity(mark);
        let maint = self.maintenance_margin(mark);

        if equity <= maint {
            self.liquidate_at(mark, ts);
        }
    }
}

// RSI
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

// tabs
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Orderbook,
    Candles,
    Data,
}

// themes
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

// load CSV data
fn load_orderbook_events(path: &str) -> Vec<OrderbookCsvEvent> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("Replay: cannot open {path}: {e}");
            return Vec::new();
        }
    };

    let reader = BufReader::new(file);
    let mut out = Vec::new();

    for (i, line) in reader.lines().enumerate() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if i == 0 && line.starts_with("ts,") {
            continue; // header
        }

        let parts: Vec<&str> = line.split(',').collect();
        if parts.len() != 5 {
            continue;
        }

        let ts = parts[0].parse::<u64>().unwrap_or(0);
        let msg_type = parts[1].to_string();
        let side = parts[2].to_string();
        let price = parts[3].parse::<f64>().unwrap_or(0.0);
        let size = parts[4].parse::<f64>().unwrap_or(0.0);

        out.push(OrderbookCsvEvent {
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

fn load_trade_events(path: &str) -> Vec<TradeCsvEvent> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("Replay: cannot open {path}: {e}");
            return Vec::new();
        }
    };

    let reader = BufReader::new(file);
    let mut out = Vec::new();

    for (i, line) in reader.lines().enumerate() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if i == 0 && line.starts_with("ts,") {
            continue;
        }

        let parts: Vec<&str> = line.split(',').collect();
        if parts.len() < 5 {
            continue;
        }

        let ts = parts[0].parse::<u64>().unwrap_or(0);
        let ticker = parts[2].to_string();
        let side = parts[3].to_string();
        let size = parts[4].parse::<f64>().unwrap_or(0.0);

        out.push(TradeCsvEvent {
            ts,
            ticker,
            side,
            size,
        });
    }

    out.sort_by_key(|e| e.ts);
    out
}

// main replay app
struct ReplayApp {
    // data
    ob_events: Vec<OrderbookCsvEvent>,
    tr_events: Vec<TradeCsvEvent>,

    // replay time
    has_data: bool,
    start_ts: u64,
    end_ts: u64,
    sim_ts: u64,
    speed: f64,
    paused: bool,
    wall_last: Instant,
    ob_index: usize,
    tr_index: usize,

    // book + candles + trading sim
    book: LiveBook,
    last_price: f64,

    tf_30s: CandleAgg,
    tf_1m: CandleAgg,
    tf_3m: CandleAgg,
    tf_5m: CandleAgg,
    selected_tf: u64,
    chart: ChartSettings,
    trading: TradingState,

    // UI state
    selected_tab: Tab,
    time_mode: TimeDisplayMode,
    current_theme: ThemeKind,
    candles_bounds: Option<PlotBounds>,

    // last trade for display
    last_trade: Option<TradeCsvEvent>,

    // data inspector config
    trades_window_secs: u64,
    events_window_secs: u64,
    max_events_rows: usize,

    // snapshot status
    snapshot_status: Option<String>,

    rng: StdRng,
}

impl ReplayApp {
    fn new() -> Self {
        let ob_events = load_orderbook_events("data/orderbook_ethusd.csv");
        let tr_events = load_trade_events("data/trades.csv");

        let has_data = !ob_events.is_empty();
        let (start_ts, end_ts) = if has_data {
            (
                ob_events.first().unwrap().ts,
                ob_events.last().unwrap().ts,
            )
        } else {
            (0, 0)
        };

        Self {
            ob_events,
            tr_events,
            has_data,
            start_ts,
            end_ts,
            sim_ts: start_ts,
            speed: 1.0,
            paused: false,
            wall_last: Instant::now(),
            ob_index: 0,
            tr_index: 0,
            book: LiveBook::default(),
            last_price: 3000.0,
            tf_30s: CandleAgg::new(30),
            tf_1m: CandleAgg::new(60),
            tf_3m: CandleAgg::new(180),
            tf_5m: CandleAgg::new(300),
            selected_tf: 60,
            chart: ChartSettings {
                y_min: 2950.0,
                y_max: 3050.0,
                show_candles: 160,
                auto_scale: true,
            },
            trading: TradingState::new(),
            selected_tab: Tab::Candles,
            time_mode: TimeDisplayMode::Local,
            current_theme: ThemeKind::ClassicDark,
            candles_bounds: None,
            last_trade: None,
            trades_window_secs: 120,
            events_window_secs: 120,
            max_events_rows: 80,
            snapshot_status: None,
            rng: StdRng::seed_from_u64(42),
        }
    }

    fn current_palette(&self) -> ThemePalette {
        theme_palette(self.current_theme)
    }

    fn apply_theme(&self, ctx: &egui::Context) {
        let pal = self.current_palette();
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

    fn format_ts(&self, ts: u64) -> String {
        format_ts_common(self.time_mode, ts)
    }

    fn current_series_for_tf(&self, tf: u64) -> Vec<Candle> {
        match tf {
            30 => self.tf_30s.get_series(),
            60 => self.tf_1m.get_series(),
            180 => self.tf_3m.get_series(),
            300 => self.tf_5m.get_series(),
            _ => self.tf_1m.get_series(),
        }
    }

    fn current_series(&self) -> Vec<Candle> {
        self.current_series_for_tf(self.selected_tf)
    }

    fn switch_tf(&mut self, new_tf: u64) {
        self.selected_tf = new_tf;
        self.candles_bounds = None;
    }

    fn reset_replay(&mut self) {
        self.book = LiveBook::default();
        self.last_price = 3000.0;
        self.tf_30s = CandleAgg::new(30);
        self.tf_1m = CandleAgg::new(60);
        self.tf_3m = CandleAgg::new(180);
        self.tf_5m = CandleAgg::new(300);
        self.trading = TradingState::new();
        self.sim_ts = self.start_ts;
        self.ob_index = 0;
        self.tr_index = 0;
        self.last_trade = None;
        self.wall_last = Instant::now();
        self.candles_bounds = None;
    }

    fn seek_to(&mut self, ts_target: u64) {
        if !self.has_data {
            return;
        }
        let target = ts_target.clamp(self.start_ts, self.end_ts);
        self.reset_replay();

        while self.ob_index < self.ob_events.len()
            && self.ob_events[self.ob_index].ts <= target
        {
            let ev = &self.ob_events[self.ob_index];

            self.book
                .apply_level(ev.side.as_str(), ev.price, ev.size);

            let (bid, ask) = self.book.best_bid_ask();
            if let (Some((bp, _)), Some((ap, _))) = (bid, ask) {
                let mid = (bp + ap) * 0.5;
                if mid > 0.0 {
                    self.last_price = mid;
                }
            }

            let volume = ev.size.abs().max(0.0);
            self.tf_30s.update(ev.ts, self.last_price, volume);
            self.tf_1m.update(ev.ts, self.last_price, volume);
            self.tf_3m.update(ev.ts, self.last_price, volume);
            self.tf_5m.update(ev.ts, self.last_price, volume);

            self.ob_index += 1;
        }

        while self.tr_index < self.tr_events.len()
            && self.tr_events[self.tr_index].ts <= target
        {
            self.last_trade = Some(self.tr_events[self.tr_index].clone());
            self.tr_index += 1;
        }

        self.sim_ts = target;
    }

    fn save_snapshot(&mut self) {
        if let Err(e) = fs::create_dir_all("data") {
            self.snapshot_status = Some(format!("snapshot: failed to create data dir: {e}"));
            return;
        }

        let path = format!("data/replay_snapshot_{}.txt", self.sim_ts);
        let (bb, ba) = self.book.best_bid_ask();

        let mut out = String::new();
        let _ = writeln!(&mut out, "sim_ts: {}", self.sim_ts);
        let _ = writeln!(&mut out, "display_time: {}", self.format_ts(self.sim_ts));

        if let Some((p, s)) = bb {
            let _ = writeln!(&mut out, "best_bid_price: {:.6}", p);
            let _ = writeln!(&mut out, "best_bid_size: {:.8}", s);
        } else {
            let _ = writeln!(&mut out, "best_bid_price: none");
            let _ = writeln!(&mut out, "best_bid_size: none");
        }

        if let Some((p, s)) = ba {
            let _ = writeln!(&mut out, "best_ask_price: {:.6}", p);
            let _ = writeln!(&mut out, "best_ask_size: {:.8}", s);
        } else {
            let _ = writeln!(&mut out, "best_ask_price: none");
            let _ = writeln!(&mut out, "best_ask_size: none");
        }

        let (mid, spread) = match (bb, ba) {
            (Some((bp, _)), Some((ap, _))) => ((bp + ap) * 0.5, ap - bp),
            _ => (0.0, 0.0),
        };
        let _ = writeln!(&mut out, "mid: {:.6}", mid);
        let _ = writeln!(&mut out, "spread: {:.6}", spread);

        let mut total_bid_size = 0.0;
        let mut total_ask_size = 0.0;
        for s in self.book.bids.values() {
            total_bid_size += *s;
        }
        for s in self.book.asks.values() {
            total_ask_size += *s;
        }
        let _ = writeln!(
            &mut out,
            "total_bid_size: {:.8}\ntotal_ask_size: {:.8}",
            total_bid_size, total_ask_size
        );

        let current = self.current_series();
        if let Some(c) = current.last() {
            let _ = writeln!(&mut out, "last_candle_tf_secs: {}", self.selected_tf);
            let _ = writeln!(&mut out, "last_candle_ts: {}", c.t);
            let _ = writeln!(&mut out, "last_candle_open: {:.6}", c.open);
            let _ = writeln!(&mut out, "last_candle_high: {:.6}", c.high);
            let _ = writeln!(&mut out, "last_candle_low: {:.6}", c.low);
            let _ = writeln!(&mut out, "last_candle_close: {:.6}", c.close);
            let _ = writeln!(&mut out, "last_candle_volume: {:.8}", c.volume);
        }

        if let Some(tr) = &self.last_trade {
            let _ = writeln!(&mut out, "last_trade_ts: {}", tr.ts);
            let _ = writeln!(&mut out, "last_trade_display: {}", self.format_ts(tr.ts));
            let _ = writeln!(&mut out, "last_trade_ticker: {}", tr.ticker);
            let _ = writeln!(&mut out, "last_trade_side: {}", tr.side);
            let _ = writeln!(&mut out, "last_trade_size: {:.8}", tr.size);
        } else {
            let _ = writeln!(&mut out, "last_trade: none");
        }

        let _ = writeln!(&mut out, "\n[BIDS]");
        for (k, s) in self.book.bids.iter().rev() {
            let p = key_to_price(*k);
            let _ = writeln!(&mut out, "{:.6}, {:.8}", p, s);
        }

        let _ = writeln!(&mut out, "\n[ASKS]");
        for (k, s) in self.book.asks.iter() {
            let p = key_to_price(*k);
            let _ = writeln!(&mut out, "{:.6}, {:.8}", p, s);
        }

        match fs::write(&path, out) {
            Ok(_) => {
                self.snapshot_status = Some(format!("snapshot saved to {}", path));
            }
            Err(e) => {
                self.snapshot_status = Some(format!("snapshot write error: {e}"));
            }
        }
    }

    fn step_sim(&mut self) {
        if !self.has_data {
            // fallback random just to keep candles alive
            let ts = self.start_ts.max(self.sim_ts).saturating_add(1);
            let step: f64 = self.rng.random_range(-2.0..2.0);
            self.last_price = (self.last_price + step).clamp(2950.0, 3050.0);
            self.tf_30s.update(ts, self.last_price, 1.0);
            self.tf_1m.update(ts, self.last_price, 1.0);
            self.tf_3m.update(ts, self.last_price, 1.0);
            self.tf_5m.update(ts, self.last_price, 1.0);
            self.sim_ts = ts;
            return;
        }

        if self.paused {
            return;
        }

        let now = Instant::now();
        let dt = now.duration_since(self.wall_last).as_secs_f64();
        self.wall_last = now;

        let sim_advance = (dt * self.speed).max(0.0);
        let new_sim_ts = ((self.sim_ts as f64) + sim_advance).min(self.end_ts as f64) as u64;
        self.sim_ts = new_sim_ts;

        // apply orderbook events up to sim_ts
        while self.ob_index < self.ob_events.len()
            && self.ob_events[self.ob_index].ts <= self.sim_ts
        {
            let ev = &self.ob_events[self.ob_index];

            self.book
                .apply_level(ev.side.as_str(), ev.price, ev.size);

            // recompute mid
            let (bid, ask) = self.book.best_bid_ask();
            if let (Some((bp, _)), Some((ap, _))) = (bid, ask) {
                let mid = (bp + ap) * 0.5;
                if mid > 0.0 {
                    self.last_price = mid;
                }
            }

            // use abs(size) as volume pulse
            let volume = ev.size.abs().max(0.0);

            self.tf_30s.update(ev.ts, self.last_price, volume);
            self.tf_1m.update(ev.ts, self.last_price, volume);
            self.tf_3m.update(ev.ts, self.last_price, volume);
            self.tf_5m.update(ev.ts, self.last_price, volume);

            self.ob_index += 1;
        }

        // apply trades up to sim_ts
        while self.tr_index < self.tr_events.len()
            && self.tr_events[self.tr_index].ts <= self.sim_ts
        {
            self.last_trade = Some(self.tr_events[self.tr_index].clone());
            self.tr_index += 1;
        }

        // update trading sim
        self.trading.check_tp_sl(self.last_price);
        self.trading.check_liquidation(self.last_price, self.sim_ts);
    }

    // UI

    fn ui_top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.selected_tab, Tab::Orderbook, "Orderbook + Depth");
            ui.selectable_value(&mut self.selected_tab, Tab::Candles, "Candles + RSI");
            ui.selectable_value(&mut self.selected_tab, Tab::Data, "Data");
            ui.separator();

            ui.label("Mode:");
            ui.label("REPLAY (offline)");

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
            ui.label("Time:");
            ui.selectable_value(&mut self.time_mode, TimeDisplayMode::Unix, "Unix");
            ui.selectable_value(&mut self.time_mode, TimeDisplayMode::Local, "Local");

            ui.separator();
            ui.label("Theme:");
            let label = self.current_theme.label();
            egui::ComboBox::from_id_source("theme_combo_replay")
                .selected_text(label)
                .show_ui(ui, |ui| {
                    for theme in ThemeKind::all() {
                        ui.selectable_value(&mut self.current_theme, *theme, theme.label());
                    }
                });

            ui.separator();
            ui.label("Replay:");
            if ui.button(if self.paused { "Play" } else { "Pause" }).clicked() {
                self.paused = !self.paused;
                self.wall_last = Instant::now();
            }
            if ui.button("Restart").clicked() {
                self.reset_replay();
            }

            ui.add(
                egui::Slider::new(&mut self.speed, 0.1..=20.0)
                    .logarithmic(true)
                    .text("speed x"),
            );

            ui.separator();
            ui.label(format!(
                "t: {} / {}",
                self.format_ts(self.sim_ts),
                if self.has_data {
                    self.format_ts(self.end_ts)
                } else {
                    "n/a".into()
                }
            ));
        });
    }

    fn ui_trading_panel(&mut self, ui: &mut egui::Ui) {
        let pal = self.current_palette();

        ui.group(|ui| {
            ui.heading("Balances + Trade (sim only, replay)");

            if self.trading.liquidated_flag {
                ui.colored_label(pal.down, "⚠ LIQUIDATED");
                if let (Some(px), Some(t)) =
                    (self.trading.last_liq_price, self.trading.last_liq_time)
                {
                    ui.label(format!("Liquidated @ {:.2} ({})", px, self.format_ts(t)));
                }
                ui.separator();
            }

            ui.label(format!("Wallet USDC: {:.2}", self.trading.wallet_usdc));
            ui.label(format!("Margin USDC: {:.2}", self.trading.margin));
            ui.separator();

            ui.horizontal(|ui| {
                ui.label("Deposit:");
                ui.add(
                    egui::DragValue::new(&mut self.trading.deposit_amount)
                        .speed(1.0)
                        .clamp_range(0.0..=1_000_000.0),
                );

                if ui.button("Deposit USDC → Margin").clicked() {
                    let amt = self.trading.deposit_amount;
                    self.trading.deposit_to_margin(amt);
                }

                ui.separator();
                ui.label("Withdraw:");
                ui.add(
                    egui::DragValue::new(&mut self.trading.withdraw_amount)
                        .speed(1.0)
                        .clamp_range(0.0..=1_000_000.0),
                );

                if ui.button("Withdraw Margin → USDC").clicked() {
                    let amt = self.trading.withdraw_amount;
                    self.trading.withdraw_from_margin(amt);
                }
            });

            ui.separator();
            ui.label(format!("Mark: {:.2}", self.last_price));
            ui.separator();

            ui.horizontal(|ui| {
                ui.label("Side:");
                for side in [PositionSide::Flat, PositionSide::Long, PositionSide::Short] {
                    if ui
                        .selectable_label(self.trading.side == side, side.label())
                        .clicked()
                    {
                        self.trading.side = side;
                    }
                }
            });

            ui.add(
                egui::Slider::new(&mut self.trading.leverage, 1.0..=50.0)
                    .text("Leverage (x)"),
            );

            let max_units = self.trading.max_position_units(self.last_price);
            if self.trading.position > max_units {
                self.trading.position = max_units;
            }

            ui.add(
                egui::Slider::new(&mut self.trading.position, 0.0..=max_units).text(format!(
                    "Position (units, max {:.4})",
                    max_units
                )),
            );

            ui.separator();

            ui.horizontal(|ui| {
                if ui.button("Open / Close (sim)").clicked() {
                    if self.trading.is_open() {
                        self.trading.close_at(self.last_price);
                    } else {
                        self.trading.open_at(self.last_price);
                    }
                }
                if ui.button("TP +1").clicked() {
                    self.trading.bump_tp(self.last_price, 1.0);
                }
                if ui.button("TP -1").clicked() {
                    self.trading.bump_tp(self.last_price, -1.0);
                }
                if ui.button("SL +1").clicked() {
                    self.trading.bump_sl(self.last_price, 1.0);
                }
                if ui.button("SL -1").clicked() {
                    self.trading.bump_sl(self.last_price, -1.0);
                }
            });

            ui.separator();

            let upnl = self.trading.unrealized_pnl(self.last_price);
            let equity = self.trading.equity(self.last_price);
            let maint = self.trading.maintenance_margin(self.last_price);

            ui.label(format!(
                "Side: {}, Pos: {:.4}, Lev: {:.2}x, Notional: {:.2}",
                self.trading.side.label(),
                self.trading.position,
                self.trading.leverage,
                self.trading.notional(),
            ));
            ui.label(format!(
                "Entry: {:.2}, uPnL: {:+.2}, rPnL: {:+.2}, Equity: {:.2}, Maint: {:.2}",
                self.trading.entry_price.unwrap_or(0.0),
                upnl,
                self.trading.realized_pnl,
                equity,
                maint
            ));
            ui.label(format!(
                "TP: {}   SL: {}",
                self.trading
                    .take_profit
                    .map(|p| format!("{:.2}", p))
                    .unwrap_or("-".into()),
                self.trading
                    .stop_loss
                    .map(|p| format!("{:.2}", p))
                    .unwrap_or("-".into()),
            ));

            ui.separator();
            ui.heading("Replay fills (from trades.csv)");

            if let Some(tr) = &self.last_trade {
                ui.label(format!(
                    "Last fill: {} | {} | {} {:.4}",
                    self.format_ts(tr.ts),
                    tr.ticker,
                    tr.side,
                    tr.size
                ));
            } else {
                ui.label("No fills yet at this replay point.");
            }
        });
    }

    fn ui_orderbook(&mut self, ui: &mut egui::Ui) {
        let avail_h = ui.available_height();
        let avail_w = ui.available_width();
        let pal = self.current_palette();

        ui.heading("Orderbook + Depth (replayed)");

        if !self.has_data {
            ui.colored_label(
                pal.down,
                "No orderbook data found in data/orderbook_ethusd.csv. Run gui_app27 first.",
            );
            return;
        }

        ui.allocate_ui(egui::vec2(avail_w, avail_h), |ui| {
            ui.horizontal(|ui| {
                let left_w = avail_w * 0.45;
                let right_w = avail_w * 0.55;

                // depth plot
                ui.allocate_ui(egui::vec2(left_w, avail_h), |ui| {
                    let mut bid_points = Vec::new();
                    let mut ask_points = Vec::new();

                    let mut cum = 0.0;
                    for (k, s) in self.book.bids.iter().rev() {
                        let p = key_to_price(*k);
                        cum += *s;
                        bid_points.push((p, cum));
                    }

                    cum = 0.0;
                    for (k, s) in self.book.asks.iter() {
                        let p = key_to_price(*k);
                        cum += *s;
                        ask_points.push((p, cum));
                    }

                    Plot::new("depth_plot_replay")
                        .height(avail_h * 0.9)
                        .allow_drag(true)
                        .allow_zoom(true)
                        .allow_scroll(true)
                        .allow_boxed_zoom(true)
                        .show(ui, |plot_ui| {
                            if !bid_points.is_empty() {
                                let pts: PlotPoints = bid_points
                                    .iter()
                                    .map(|(x, y)| [*x, *y])
                                    .collect::<Vec<_>>()
                                    .into();
                                plot_ui
                                    .line(Line::new(pts).name("Bids").color(pal.depth_bid));
                            }
                            if !ask_points.is_empty() {
                                let pts: PlotPoints = ask_points
                                    .iter()
                                    .map(|(x, y)| [*x, *y])
                                    .collect::<Vec<_>>()
                                    .into();
                                plot_ui
                                    .line(Line::new(pts).name("Asks").color(pal.depth_ask));
                            }
                        });
                });

                ui.separator();

                // ladder + trading panel
                ui.allocate_ui(egui::vec2(right_w, avail_h), |ui| {
                    ui.label("Top ladders (ETH-USD replay)");

                    ui.columns(2, |cols| {
                        cols[0].label("Bids");
                        egui::Grid::new("bids_grid_replay")
                            .striped(true)
                            .show(&mut cols[0], |ui| {
                                ui.label("Price");
                                ui.label("Size");
                                ui.end_row();
                                for (k, s) in self.book.bids.iter().rev().take(15) {
                                    let p = key_to_price(*k);
                                    ui.label(format!("{:>8.2}", p));
                                    ui.label(format!("{:>6.4}", s));
                                    ui.end_row();
                                }
                            });

                        cols[1].label("Asks");
                        egui::Grid::new("asks_grid_replay")
                            .striped(true)
                            .show(&mut cols[1], |ui| {
                                ui.label("Price");
                                ui.label("Size");
                                ui.end_row();
                                for (k, s) in self.book.asks.iter().take(15) {
                                    let p = key_to_price(*k);
                                    ui.label(format!("{:>8.2}", p));
                                    ui.label(format!("{:>6.4}", s));
                                    ui.end_row();
                                }
                            });
                    });

                    ui.separator();
                    self.ui_trading_panel(ui);
                });
            });
        });
    }

    fn ui_candles(&mut self, ui: &mut egui::Ui) {
        let series_vec = self.current_series();
        if series_vec.is_empty() {
            ui.label("No candles yet (need data/orderbook_ethusd.csv).");
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

        // candles plot (filled bodies, tuned wheel, time axis formatter)
        ui.allocate_ui(egui::vec2(avail_w, candles_h), |ui| {
            let mode = self.time_mode;
            let ctx = ui.ctx().clone();
            let prev_bounds = self.candles_bounds;

            let mut new_bounds_out: Option<PlotBounds> = None;

            Plot::new("candles_plot_replay")
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

                        // wick
                        let wick_pts: PlotPoints =
                            vec![[mid, c.low], [mid, c.high]].into();
                        plot_ui.line(Line::new(wick_pts).color(color));

                        // filled body
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

                    if let Some(entry) = self.trading.entry_price {
                        plot_ui.hline(HLine::new(entry).name("entry"));
                    }
                    if let Some(tp) = self.trading.take_profit {
                        plot_ui.hline(HLine::new(tp).name("TP"));
                    }
                    if let Some(sl) = self.trading.stop_loss {
                        plot_ui.hline(HLine::new(sl).name("SL"));
                    }
                    if let Some(liq_px) = self.trading.last_liq_price {
                        plot_ui.hline(HLine::new(liq_px).name("LIQ"));
                    }

                    let mut bounds = plot_ui.plot_bounds();

                    if let Some(prev) = prev_bounds {
                        let mut restore_x = false;
                        let mut restore_y = false;

                        ctx.input(|i| {
                            if i.raw_scroll_delta.y != 0.0 {
                                if i.modifiers.shift {
                                    restore_x = true; // Shift + wheel => Y zoom only
                                } else {
                                    restore_y = true; // wheel => X zoom only
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
            Plot::new("volume_plot_replay")
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
            Plot::new("rsi_plot_replay")
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

        // bottom info + trading
        ui.allocate_ui(egui::vec2(avail_w, bottom_h), |ui| {
            ui.columns(2, |cols| {
                cols[0].group(|ui| {
                    ui.label("Last candle:");
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

                self.ui_trading_panel(&mut cols[1]);
            });
        });
    }

    fn ui_data(&mut self, ui: &mut egui::Ui) {
        let pal = self.current_palette();

        ui.heading("Data Inspector (current replay state)");

        if !self.has_data {
            ui.colored_label(
                pal.down,
                "No data loaded. Need data/orderbook_ethusd.csv (and optionally data/trades.csv).",
            );
            return;
        }

        ui.horizontal(|ui| {
            ui.label(format!(
                "Replay time: {} (unix {})",
                self.format_ts(self.sim_ts),
                self.sim_ts
            ));
            ui.separator();
            if ui.button("Save snapshot").clicked() {
                self.save_snapshot();
            }
            if let Some(msg) = &self.snapshot_status {
                ui.label(msg);
            }
        });

        ui.separator();

        // ---- Orderbook summary ----
        let (bb, ba) = self.book.best_bid_ask();
        let mut total_bid_size = 0.0;
        let mut total_ask_size = 0.0;
        for s in self.book.bids.values() {
            total_bid_size += *s;
        }
        for s in self.book.asks.values() {
            total_ask_size += *s;
        }

        let (mid, spread) = match (bb, ba) {
            (Some((bp, _)), Some((ap, _))) => ((bp + ap) * 0.5, ap - bp),
            _ => (0.0, 0.0),
        };

        ui.group(|ui| {
            ui.heading("Orderbook snapshot (ETH-USD)");
            ui.label(format!("# bid levels: {}", self.book.bids.len()));
            ui.label(format!("# ask levels: {}", self.book.asks.len()));
            if let Some((bp, bs)) = bb {
                ui.label(format!("Best bid: {:.4} (size {:.6})", bp, bs));
            } else {
                ui.label("Best bid: none");
            }
            if let Some((ap, asz)) = ba {
                ui.label(format!("Best ask: {:.4} (size {:.6})", ap, asz));
            } else {
                ui.label("Best ask: none");
            }
            ui.label(format!("Mid: {:.4}", mid));
            ui.label(format!("Spread: {:.4}", spread));
            ui.label(format!(
                "Total bid size: {:.6} | Total ask size: {:.6}",
                total_bid_size, total_ask_size
            ));
        });

        ui.separator();

        // ---- Full ladders / raw levels ----
        egui::CollapsingHeader::new("Full ladder snapshot (all levels)")
            .default_open(false)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(300.0)
                    .show(ui, |ui| {
                        ui.columns(2, |cols| {
                            cols[0].label("Bids (sorted descending)");
                            egui::Grid::new("data_bids_grid")
                                .striped(true)
                                .show(&mut cols[0], |ui| {
                                    ui.label("Price");
                                    ui.label("Size");
                                    ui.end_row();
                                    for (k, s) in self.book.bids.iter().rev() {
                                        let p = key_to_price(*k);
                                        ui.label(format!("{:>10.4}", p));
                                        ui.label(format!("{:>10.6}", s));
                                        ui.end_row();
                                    }
                                });

                            cols[1].label("Asks (sorted ascending)");
                            egui::Grid::new("data_asks_grid")
                                .striped(true)
                                .show(&mut cols[1], |ui| {
                                    ui.label("Price");
                                    ui.label("Size");
                                    ui.end_row();
                                    for (k, s) in self.book.asks.iter() {
                                        let p = key_to_price(*k);
                                        ui.label(format!("{:>10.4}", p));
                                        ui.label(format!("{:>10.6}", s));
                                        ui.end_row();
                                    }
                                });
                        });
                    });
            });

        ui.separator();

        // ---- TF candle summary ----
        egui::CollapsingHeader::new("Candle snapshot by timeframe")
            .default_open(false)
            .show(ui, |ui| {
                let tfs = [30_u64, 60, 180, 300];
                egui::Grid::new("tf_candle_grid")
                    .striped(true)
                    .show(ui, |ui| {
                        ui.label("TF (s)");
                        ui.label("ts (unix)");
                        ui.label("ts (display)");
                        ui.label("O");
                        ui.label("H");
                        ui.label("L");
                        ui.label("C");
                        ui.label("V");
                        ui.end_row();

                        for tf in tfs {
                            let series = self.current_series_for_tf(tf);
                            if let Some(c) = series.last() {
                                ui.label(format!("{}", tf));
                                ui.label(format!("{}", c.t));
                                ui.label(self.format_ts(c.t));
                                ui.label(format!("{:.4}", c.open));
                                ui.label(format!("{:.4}", c.high));
                                ui.label(format!("{:.4}", c.low));
                                ui.label(format!("{:.4}", c.close));
                                ui.label(format!("{:.6}", c.volume));
                            } else {
                                ui.label(format!("{}", tf));
                                ui.label("-");
                                ui.label("-");
                                ui.label("-");
                                ui.label("-");
                                ui.label("-");
                                ui.label("-");
                                ui.label("-");
                            }
                            ui.end_row();
                        }
                    });
            });

        ui.separator();

        // ---- Recent trades ----
        egui::CollapsingHeader::new("Recent trades around current time")
            .default_open(true)
            .show(ui, |ui| {
                let window_secs = self.trades_window_secs.max(10);
                let lower = self.sim_ts.saturating_sub(window_secs);

                let mut rows: Vec<&TradeCsvEvent> = self
                    .tr_events
                    .iter()
                    .filter(|tr| tr.ts >= lower && tr.ts <= self.sim_ts)
                    .collect();

                rows.sort_by_key(|tr| tr.ts);

                ui.horizontal(|ui| {
                    ui.label("Trade window (s):");
                    ui.add(
                        egui::DragValue::new(&mut self.trades_window_secs)
                            .speed(5)
                            .clamp_range(10..=86_400),
                    );
                    if !rows.is_empty() {
                        if ui.button("Jump to last trade in window").clicked() {
                            if let Some(last) = rows.last() {
                                self.seek_to(last.ts);
                            }
                        }
                    }
                });

                if rows.is_empty() {
                    ui.label("No trades in selected window.");
                    return;
                }

                egui::ScrollArea::vertical()
                    .max_height(220.0)
                    .show(ui, |ui| {
                        egui::Grid::new("data_trades_grid")
                            .striped(true)
                            .show(ui, |ui| {
                                ui.label("ts (unix)");
                                ui.label("ts (display)");
                                ui.label("ticker");
                                ui.label("side");
                                ui.label("size");
                                ui.end_row();

                                for tr in rows {
                                    ui.label(format!("{}", tr.ts));
                                    ui.label(self.format_ts(tr.ts));
                                    ui.label(&tr.ticker);
                                    ui.label(&tr.side);
                                    ui.label(format!("{:.6}", tr.size));
                                    ui.end_row();
                                }
                            });
                    });
            });

        ui.separator();

        // ---- Recent raw orderbook events ----
        egui::CollapsingHeader::new("Recent raw orderbook events")
            .default_open(false)
            .show(ui, |ui| {
                let window_secs = self.events_window_secs.max(10);
                let lower = self.sim_ts.saturating_sub(window_secs);

                let mut rows: Vec<&OrderbookCsvEvent> = self
                    .ob_events
                    .iter()
                    .filter(|ev| ev.ts >= lower && ev.ts <= self.sim_ts)
                    .collect();

                rows.sort_by_key(|ev| ev.ts);

                let max_rows = self.max_events_rows.max(10);
                if rows.len() > max_rows {
                    let start = rows.len() - max_rows;
                    rows = rows[start..].to_vec();
                }

                ui.horizontal(|ui| {
                    ui.label("Event window (s):");
                    ui.add(
                        egui::DragValue::new(&mut self.events_window_secs)
                            .speed(5)
                            .clamp_range(10..=86_400),
                    );
                    ui.label("Max rows:");
                    ui.add(
                        egui::DragValue::new(&mut self.max_events_rows)
                            .speed(5)
                            .clamp_range(10..=1000),
                    );
                });

                if rows.is_empty() {
                    ui.label("No orderbook events in selected window.");
                    return;
                }

                egui::ScrollArea::vertical()
                    .max_height(260.0)
                    .show(ui, |ui| {
                        egui::Grid::new("data_ob_events_grid")
                            .striped(true)
                            .show(ui, |ui| {
                                ui.label("ts (unix)");
                                ui.label("ts (display)");
                                ui.label("type");
                                ui.label("side");
                                ui.label("price");
                                ui.label("size");
                                ui.end_row();

                                for ev in rows {
                                    ui.label(format!("{}", ev.ts));
                                    ui.label(self.format_ts(ev.ts));
                                    ui.label(&ev.msg_type);
                                    ui.label(&ev.side);
                                    ui.label(format!("{:.4}", ev.price));
                                    ui.label(format!("{:.6}", ev.size));
                                    ui.end_row();
                                }
                            });
                    });
            });
    }
}

impl eframe::App for ReplayApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.apply_theme(ctx);
        self.step_sim();

        egui::TopBottomPanel::top("top_panel_replay").show(ctx, |ui| {
            self.ui_top_bar(ui);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| match self.selected_tab {
                    Tab::Orderbook => self.ui_orderbook(ui),
                    Tab::Candles => self.ui_candles(ui),
                    Tab::Data => self.ui_data(ui),
                });
        });

        ctx.request_repaint_after(Duration::from_millis(33));
    }
}

fn main() {
    // ensure data dir exists (just so writes from live app have a home)
    let _ = fs::create_dir_all("data");

    let options = eframe::NativeOptions::default();
    if let Err(e) = eframe::run_native(
        "Ladder REPLAY (offline)",
        options,
        Box::new(|_cc| Box::new(ReplayApp::new())),
    ) {
        eprintln!("eframe error: {e}");
    }
}
