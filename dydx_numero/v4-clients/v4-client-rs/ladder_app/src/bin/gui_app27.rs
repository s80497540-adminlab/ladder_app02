// ladder_app/src/bin/gui_app27.rs
//
// gui_app with persistence + themes + mouse-tuned chart:
//
//  - Saves ALL orderbook bids/asks from the testnet orders feed to data/orderbook_ethusd.csv
//  - Saves all REAL orders placed from the GUI to data/trades.csv
//
// Existing features:
//   * Themes (5 palettes)
//   * Ticker dropdown (ETH-USD / BTC-USD / SOL-USD)
//   * Unix vs Local time toggle (axes + labels)
//   * Volume histogram
//   * RSI
//   * Scrollable layout
//   * REAL testnet market orders + subaccount feed
//   * LIVE orderbook + depth
//   * Fake trading sim
//   * Filled candle bodies with width proportional to TF
//   * Mouse-tuned candles plot:
//       - Wheel: X zoom only
//       - Shift + wheel: Y zoom only
//       - Drag: pan X & Y
//
// Run:
//   export DYDX_TESTNET_MNEMONIC='mirror actor ... render wait'
//   cargo run -p ladder_app --bin gui_app27

mod candle_agg;

use candle_agg::{Candle, CandleAgg};

use eframe::egui;
use egui::{Color32, Stroke};
use egui_plot::{GridMark, HLine, Line, Plot, PlotBounds, PlotPoints, Polygon, VLine};

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::{mpsc, watch, Mutex};

// ---- dYdX types ----
use bigdecimal::BigDecimal;
use bigdecimal::Zero;

use dydx_client::config::ClientConfig;
use dydx_client::indexer::{
    Feed as DxFeed, Feeds, IndexerClient, OrdersMessage, OrderbookResponsePriceLevel, Price,
    Quantity, SubaccountsMessage, Ticker,
};
use dydx_client::node::{NodeClient, OrderBuilder, OrderSide, Wallet};
use dydx_proto::dydxprotocol::clob::order::TimeInForce;

// time handling
use chrono::{Local, TimeZone};

// ---- rustls crypto provider ----
fn init_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

// ---- LIVE BOOK (from dYdX testnet) ----
#[derive(Default, Clone, Debug)]
pub struct LiveBook {
    pub bids: BTreeMap<Price, Quantity>,
    pub asks: BTreeMap<Price, Quantity>,
}

impl LiveBook {
    fn apply_levels(map: &mut BTreeMap<Price, Quantity>, levels: Vec<OrderbookResponsePriceLevel>) {
        for lvl in levels {
            let p = lvl.price;
            let s = lvl.size;
            if s.0.is_zero() {
                map.remove(&p);
            } else {
                map.insert(p, s);
            }
        }
    }

    pub fn apply_initial(
        &mut self,
        bids: Vec<OrderbookResponsePriceLevel>,
        asks: Vec<OrderbookResponsePriceLevel>,
    ) {
        self.bids.clear();
        self.asks.clear();
        Self::apply_levels(&mut self.bids, bids);
        Self::apply_levels(&mut self.asks, asks);
    }

    pub fn apply_update(
        &mut self,
        bids: Option<Vec<OrderbookResponsePriceLevel>>,
        asks: Option<Vec<OrderbookResponsePriceLevel>>,
    ) {
        if let Some(b) = bids {
            Self::apply_levels(&mut self.bids, b);
        }
        if let Some(a) = asks {
            Self::apply_levels(&mut self.asks, a);
        }
    }
}

// quick BigDecimal -> f64 for UI (fine for now)
fn bd_to_f64(bd: &bigdecimal::BigDecimal) -> f64 {
    bd.to_string().parse::<f64>().unwrap_or(0.0)
}
fn price_to_f64(p: &Price) -> f64 {
    bd_to_f64(&p.0)
}
fn qty_to_f64(q: &Quantity) -> f64 {
    bd_to_f64(&q.0)
}

// ---- chart settings ----
#[derive(Clone)]
struct ChartSettings {
    y_min: f64,
    y_max: f64,
    show_candles: usize,
    auto_scale: bool,
}

// ---- trading sim ----
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
    // balances
    wallet_usdc: f64,
    margin: f64,

    // transfer inputs
    deposit_amount: f64,
    withdraw_amount: f64,

    // position
    leverage: f64,
    position: f64,
    side: PositionSide,
    entry_price: Option<f64>,
    realized_pnl: f64,
    take_profit: Option<f64>,
    stop_loss: Option<f64>,

    // liquidation
    maint_rate: f64,
    last_liq_price: Option<f64>,
    last_liq_time: Option<u64>,
    liquidated_flag: bool,

    // REAL trade size (ticker comes from MyApp.current_ticker)
    real_size_units: f64,
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

            real_size_units: 0.02,
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

        // apply pnl to margin
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

        // wipe remaining margin
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

// ---- RSI helper ----
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

// ---- REAL trading channels ----
#[derive(Debug, Clone)]
enum TradeCmd {
    MarketBuy { ticker: String, size: BigDecimal },
    MarketSell { ticker: String, size: BigDecimal },
}

#[derive(Debug, Clone, Default)]
struct AccountState {
    last_snapshot: Option<String>,
    last_update: Option<String>,
    last_tx: Option<String>,
    last_error: Option<String>,
}

// ---- tabs ----
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Orderbook,
    Candles,
}

// ---- time display mode ----
#[derive(Clone, Copy, PartialEq, Eq)]
enum TimeDisplayMode {
    Unix,
    Local,
}

// ---- themes ----
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
    // chart colors
    up: Color32,
    down: Color32,
    depth_bid: Color32,
    depth_ask: Color32,
    volume_up: Color32,
    volume_down: Color32,
    rsi_line: Color32,
    // ui colors
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

// shared time formatter
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

struct MyApp {
    selected_tab: Tab,
    last_price: f64,

    tf_30s: CandleAgg,
    tf_1m: CandleAgg,
    tf_3m: CandleAgg,
    tf_5m: CandleAgg,
    selected_tf: u64,

    chart: ChartSettings,
    trading: TradingState,

    // live book
    live_book_rx: watch::Receiver<LiveBook>,
    live_book: LiveBook,

    // real trading
    trade_tx: mpsc::Sender<TradeCmd>,
    acct_rx: watch::Receiver<AccountState>,
    acct_state: AccountState,

    // ticker handling
    available_tickers: Vec<String>,
    current_ticker_index: usize,

    // time mode
    time_mode: TimeDisplayMode,

    // theme
    current_theme: ThemeKind,

    // candle plot bounds (for custom wheel behavior)
    candles_bounds: Option<PlotBounds>,

    rng: StdRng,
}

impl MyApp {
    fn new(
        live_book_rx: watch::Receiver<LiveBook>,
        trade_tx: mpsc::Sender<TradeCmd>,
        acct_rx: watch::Receiver<AccountState>,
    ) -> Self {
        Self {
            selected_tab: Tab::Candles,
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

            live_book: LiveBook::default(),
            live_book_rx,

            trade_tx,
            acct_rx,
            acct_state: AccountState::default(),

            available_tickers: vec![
                "ETH-USD".to_string(),
                "BTC-USD".to_string(),
                "SOL-USD".to_string(),
            ],
            current_ticker_index: 0,

            time_mode: TimeDisplayMode::Local,
            current_theme: ThemeKind::ClassicDark,

            candles_bounds: None,

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

    fn current_ticker(&self) -> &str {
        if self.available_tickers.is_empty() {
            "ETH-USD"
        } else {
            let idx = self
                .current_ticker_index
                .min(self.available_tickers.len() - 1);
            &self.available_tickers[idx]
        }
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
        // when switching TF, forget custom bounds so plot recalculates naturally
        self.candles_bounds = None;
    }

    fn tick(&mut self) {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // pull live book if changed
        let mut have_live_mid = false;
        if self.live_book_rx.has_changed().unwrap_or(false) {
            self.live_book = self.live_book_rx.borrow().clone();

            if let (Some((bp, _)), Some((ap, _))) = (
                self.live_book.bids.iter().next_back(),
                self.live_book.asks.iter().next(),
            ) {
                let mid = (price_to_f64(bp) + price_to_f64(ap)) * 0.5;
                if mid > 0.0 {
                    self.last_price = mid;
                    have_live_mid = true;
                }
            }
        }

        // fallback random walk ONLY when no live mid yet
        if !have_live_mid {
            let step: f64 = self.rng.random_range(-2.0..2.0);
            self.last_price = (self.last_price + step).clamp(2950.0, 3050.0);
        }

        // pull account state if changed
        if self.acct_rx.has_changed().unwrap_or(false) {
            self.acct_state = self.acct_rx.borrow().clone();
        }

        // fake sim checks
        self.trading.check_tp_sl(self.last_price);
        self.trading.check_liquidation(self.last_price, ts);

        // dummy volume for now (can be wired to real trades later)
        let volume = 1.0;

        // update candle aggs
        self.tf_30s.update(ts, self.last_price, volume);
        self.tf_1m.update(ts, self.last_price, volume);
        self.tf_3m.update(ts, self.last_price, volume);
        self.tf_5m.update(ts, self.last_price, volume);
    }

    // ---- UI ----
    fn ui_top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.selected_tab, Tab::Orderbook, "Orderbook + Depth");
            ui.selectable_value(&mut self.selected_tab, Tab::Candles, "Candles + RSI");
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
            ui.label("Ticker:");
            let selected = self.current_ticker().to_string();
            egui::ComboBox::from_id_source("ticker_combo")
                .selected_text(selected.clone())
                .show_ui(ui, |ui| {
                    for (i, t) in self.available_tickers.iter().enumerate() {
                        ui.selectable_value(&mut self.current_ticker_index, i, t);
                    }
                });

            ui.separator();
            ui.label("Time:");
            ui.selectable_value(&mut self.time_mode, TimeDisplayMode::Unix, "Unix");
            ui.selectable_value(&mut self.time_mode, TimeDisplayMode::Local, "Local");

            ui.separator();
            ui.label("Theme:");
            let label = self.current_theme.label();
            egui::ComboBox::from_id_source("theme_combo")
                .selected_text(label)
                .show_ui(ui, |ui| {
                    for theme in ThemeKind::all() {
                        ui.selectable_value(&mut self.current_theme, *theme, theme.label());
                    }
                });

            ui.separator();
            ui.checkbox(&mut self.chart.auto_scale, "Auto Y");
            ui.separator();
            ui.label(format!("Mark: {:.2}", self.last_price));
        });
    }

    fn ui_trading_panel(&mut self, ui: &mut egui::Ui, real_ticker: &str) {
        let pal = self.current_palette();

        ui.group(|ui| {
            ui.heading("Balances + Trade (fake + real)");

            if self.trading.liquidated_flag {
                ui.colored_label(pal.down, "⚠ LIQUIDATED");
                if let (Some(px), Some(t)) =
                    (self.trading.last_liq_price, self.trading.last_liq_time)
                {
                    ui.label(format!("Liquidated @ {:.2} (t={})", px, self.format_ts(t)));
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
                if ui.button("Open / Close (fake)").clicked() {
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

            // ---- REAL testnet controls ----
            ui.separator();
            ui.heading("REAL testnet order (market)");

            ui.horizontal(|ui| {
                ui.label("Ticker (dropdown):");
                ui.colored_label(pal.accent, real_ticker);

                ui.label("Size (units):");
                ui.add(
                    egui::DragValue::new(&mut self.trading.real_size_units)
                        .speed(0.001)
                        .clamp_range(0.0..=1000.0),
                );
            });

            ui.horizontal(|ui| {
                if ui.button("Market BUY (real)").clicked() {
                    let t = real_ticker.trim().to_string();
                    let s_str = format!("{:.8}", self.trading.real_size_units.max(0.0));
                    if let Ok(size) = BigDecimal::from_str(&s_str) {
                        let _ =
                            self.trade_tx
                                .try_send(TradeCmd::MarketBuy { ticker: t, size });
                    }
                }
                if ui.button("Market SELL (real)").clicked() {
                    let t = real_ticker.trim().to_string();
                    let s_str = format!("{:.8}", self.trading.real_size_units.max(0.0));
                    if let Ok(size) = BigDecimal::from_str(&s_str) {
                        let _ =
                            self.trade_tx
                                .try_send(TradeCmd::MarketSell { ticker: t, size });
                    }
                }
            });

            if let Some(tx) = &self.acct_state.last_tx {
                ui.label(format!("Last tx: {tx}"));
            }
            if let Some(err) = &self.acct_state.last_error {
                ui.colored_label(pal.down, format!("Last error: {err}"));
            }
        });
    }

    fn ui_orderbook(&mut self, ui: &mut egui::Ui) {
        let avail_h = ui.available_height();
        let avail_w = ui.available_width();

        let pal = self.current_palette();

        ui.heading("Orderbook + Depth (LIVE testnet)");

        ui.allocate_ui(egui::vec2(avail_w, avail_h), |ui| {
            ui.horizontal(|ui| {
                let left_w = avail_w * 0.45;
                let right_w = avail_w * 0.55;

                // ---- depth plot ----
                ui.allocate_ui(egui::vec2(left_w, avail_h), |ui| {
                    let mut bid_points = Vec::new();
                    let mut ask_points = Vec::new();

                    let mut cum = 0.0;
                    for (p, s) in self.live_book.bids.iter().rev() {
                        cum += qty_to_f64(s);
                        bid_points.push((price_to_f64(p), cum));
                    }

                    cum = 0.0;
                    for (p, s) in self.live_book.asks.iter() {
                        cum += qty_to_f64(s);
                        ask_points.push((price_to_f64(p), cum));
                    }

                    Plot::new("depth_plot")
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
                                plot_ui.line(Line::new(pts).name("Bids").color(pal.depth_bid));
                            }
                            if !ask_points.is_empty() {
                                let pts: PlotPoints = ask_points
                                    .iter()
                                    .map(|(x, y)| [*x, *y])
                                    .collect::<Vec<_>>()
                                    .into();
                                plot_ui.line(Line::new(pts).name("Asks").color(pal.depth_ask));
                            }
                        });
                });

                ui.separator();

                // ---- ladder + trading panel ----
                ui.allocate_ui(egui::vec2(right_w, avail_h), |ui| {
                    ui.label("Top ladders");

                    ui.columns(2, |cols| {
                        cols[0].label("Bids");
                        egui::Grid::new("bids_grid")
                            .striped(true)
                            .show(&mut cols[0], |ui| {
                                ui.label("Price");
                                ui.label("Size");
                                ui.end_row();
                                for (p, s) in self.live_book.bids.iter().rev().take(15) {
                                    ui.label(format!("{:>8.2}", price_to_f64(p)));
                                    ui.label(format!("{:>6.4}", qty_to_f64(s)));
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
                                for (p, s) in self.live_book.asks.iter().take(15) {
                                    ui.label(format!("{:>8.2}", price_to_f64(p)));
                                    ui.label(format!("{:>6.4}", qty_to_f64(s)));
                                    ui.end_row();
                                }
                            });
                    });

                    ui.separator();
                    let ticker = self.current_ticker().to_string();
                    self.ui_trading_panel(ui, &ticker);
                });
            });
        });
    }

    fn ui_candles(&mut self, ui: &mut egui::Ui) {
        let series_vec = self.current_series();
        if series_vec.is_empty() {
            ui.label("No candles yet.");
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
                ui.add(egui::DragValue::new(&mut self.chart.y_min).speed(1.0).prefix("min "));
                ui.add(egui::DragValue::new(&mut self.chart.y_max).speed(1.0).prefix("max "));
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

        // keep manual Y values in sync when auto_scale is enabled
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

        // ---- candles plot (FILLED BODIES, WIDTH RELATIVE TO TF, tuned mouse) ----
        ui.allocate_ui(egui::vec2(avail_w, candles_h), |ui| {
            let mode = self.time_mode;
            let ctx = ui.ctx().clone();
            let prev_bounds = self.candles_bounds;

            let mut new_bounds_out: Option<PlotBounds> = None;

            Plot::new("candles_plot")
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
                    // draw candles
                    for c in visible {
                        let mid = c.t as f64 + tf * 0.5;

                        // body width = 70% of TF
                        let body_half = tf * 0.35;
                        let body_left = mid - body_half;
                        let body_right = mid + body_half;

                        let top = c.open.max(c.close);
                        let bot = c.open.min(c.close);

                        let color = if c.close >= c.open { pal.up } else { pal.down };

                        // wick (vertical line)
                        let wick_pts: PlotPoints = vec![[mid, c.low], [mid, c.high]].into();
                        plot_ui.line(Line::new(wick_pts).color(color));

                        // FILLED body as polygon rectangle with relative width
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

                    // rulers: current price/time
                    let now_x = last.t as f64 + tf;
                    let now_px = last.close;
                    plot_ui.hline(HLine::new(now_px).name("now_px"));
                    plot_ui.vline(VLine::new(now_x).name("now_t"));

                    // position lines (fake sim)
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

                    // ---- custom wheel behavior: wheel = X zoom, Shift+wheel = Y zoom ----
                    let mut bounds = plot_ui.plot_bounds();

                    if let Some(prev) = prev_bounds {
                        let mut restore_x = false;
                        let mut restore_y = false;

                        ctx.input(|i| {
                            if i.raw_scroll_delta.y != 0.0 {
                                if i.modifiers.shift {
                                    // Shift + wheel => Y zoom only => restore X
                                    restore_x = true;
                                } else {
                                    // Wheel => X zoom only => restore Y
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

        // ---- Volume plot ----
        ui.allocate_ui(egui::vec2(avail_w, volume_h), |ui| {
            let max_vol = visible
                .iter()
                .map(|c| c.volume)
                .fold(0.0_f64, f64::max)
                .max(1.0);

            let mode = self.time_mode;
            let pal = self.current_palette();
            Plot::new("volume_plot")
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

                        let color =
                            if c.close >= c.open { pal.volume_up } else { pal.volume_down };

                        let pts: PlotPoints = vec![[mid, 0.0], [mid, v]].into();
                        plot_ui.line(Line::new(pts).color(color).width(2.0));
                    }
                });
        });

        ui.separator();

        // ---- RSI plot ----
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
            Plot::new("rsi_plot")
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

        // ---- bottom info + trading ----
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

                let ticker = self.current_ticker().to_string();
                self.ui_trading_panel(&mut cols[1], &ticker);
            });
        });
    }
}

impl eframe::App for MyApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // apply theme first so everything uses it
        self.apply_theme(ctx);

        self.tick();

        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            self.ui_top_bar(ui);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| match self.selected_tab {
                    Tab::Orderbook => self.ui_orderbook(ui),
                    Tab::Candles => self.ui_candles(ui),
                });
        });

        ctx.request_repaint_after(Duration::from_millis(33));
    }
}

// ---- main: spawn testnet tasks and run GUI ----
fn main() {
    init_crypto_provider();

    // make sure data dir exists
    fs::create_dir_all("data").ok();

    // live orderbook watch channel
    let (book_tx, book_rx) = watch::channel(LiveBook::default());

    // real trading channels
    let (trade_tx, trade_rx) = mpsc::channel::<TradeCmd>(32);
    let (acct_tx, acct_rx) = watch::channel::<AccountState>(AccountState::default());

    // shared account state for feed + trade loop
    let shared_state = Arc::new(Mutex::new(AccountState::default()));

    // Tokio runtime that lives alongside GUI
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio rt");

    // Task 1: dYdX testnet orderbook deltas (ETH-USD feed as driver) + persistence
    rt.spawn(async move {
        // open file for orderbook logging
        let mut ob_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open("data/orderbook_ethusd.csv")
            .expect("open orderbook_ethusd.csv");

        // optional header (will repeat across runs, harmless)
        let _ = writeln!(ob_file, "ts,msg_type,side,price,size");

        let config = ClientConfig::from_file("client/tests/testnet.toml")
            .await
            .expect("load testnet.toml");

        let mut indexer = IndexerClient::new(config.indexer);
        let mut feeds: Feeds<'_> = indexer.feed();

        let ticker = Ticker("ETH-USD".to_string());
        let mut feed: DxFeed<OrdersMessage> =
            feeds.orders(&ticker, false).await.expect("orders feed");

        let mut book = LiveBook::default();

        while let Some(msg) = feed.recv().await {
            let ts = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();

            match &msg {
                OrdersMessage::Initial(init) => {
                    // log bids
                    for lvl in &init.contents.bids {
                        let price = bd_to_f64(&lvl.price.0);
                        let size = bd_to_f64(&lvl.size.0);
                        let _ = writeln!(ob_file, "{ts},initial,bid,{price},{size}");
                    }
                    // log asks
                    for lvl in &init.contents.asks {
                        let price = bd_to_f64(&lvl.price.0);
                        let size = bd_to_f64(&lvl.size.0);
                        let _ = writeln!(ob_file, "{ts},initial,ask,{price},{size}");
                    }

                    book.apply_initial(
                        init.contents.bids.clone(),
                        init.contents.asks.clone(),
                    );
                }
                OrdersMessage::Update(upd) => {
                    if let Some(bids) = &upd.contents.bids {
                        for lvl in bids {
                            let price = bd_to_f64(&lvl.price.0);
                            let size = bd_to_f64(&lvl.size.0);
                            let _ = writeln!(ob_file, "{ts},update,bid,{price},{size}");
                        }
                    }
                    if let Some(asks) = &upd.contents.asks {
                        for lvl in asks {
                            let price = bd_to_f64(&lvl.price.0);
                            let size = bd_to_f64(&lvl.size.0);
                            let _ = writeln!(ob_file, "{ts},update,ask,{price},{size}");
                        }
                    }

                    book.apply_update(
                        upd.contents.bids.clone(),
                        upd.contents.asks.clone(),
                    );
                }
            }

            let _ = ob_file.flush();
            let _ = book_tx.send(book.clone());
        }
    });

    // Task 2: REAL trading + subaccount feed + trades logging
    rt.spawn({
        let shared_state = shared_state.clone();
        async move {
            // trades log file
            let mut trades_file = OpenOptions::new()
                .create(true)
                .append(true)
                .open("data/trades.csv")
                .expect("open trades.csv");
            let _ = writeln!(trades_file, "ts,tx_hash,ticker,side,size");

            let config = match ClientConfig::from_file("client/tests/testnet.toml").await {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("REAL trading disabled: failed to load testnet.toml: {e}");
                    return;
                }
            };

            let raw = match env::var("DYDX_TESTNET_MNEMONIC") {
                Ok(v) => v,
                Err(_) => {
                    eprintln!("REAL trading disabled: DYDX_TESTNET_MNEMONIC not set");
                    return;
                }
            };
            let mnemonic = raw.split_whitespace().collect::<Vec<_>>().join(" ");

            let wallet = match Wallet::from_mnemonic(&mnemonic) {
                Ok(w) => w,
                Err(e) => {
                    eprintln!("REAL trading disabled: invalid mnemonic: {e}");
                    return;
                }
            };

            let mut node = match NodeClient::connect(config.node).await {
                Ok(n) => n,
                Err(e) => {
                    eprintln!("REAL trading disabled: node connect failed: {e}");
                    return;
                }
            };

            let mut account = match wallet.account(0, &mut node).await {
                Ok(a) => a,
                Err(e) => {
                    eprintln!("REAL trading disabled: account sync failed: {e}");
                    return;
                }
            };

            // subaccount for feed subscription
            let sub_for_feed = match account.subaccount(0) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("REAL trading disabled: subaccount derive failed: {e}");
                    return;
                }
            };

            let mut indexer = IndexerClient::new(config.indexer);
            let mut feeds = indexer.feed();

            let mut sub_feed = match feeds.subaccounts(sub_for_feed, false).await {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("REAL trading disabled: subaccounts feed failed: {e}");
                    return;
                }
            };

            // Feed listener: updates snapshot/update fields
            let acct_tx_feed = acct_tx.clone();
            let shared_state_feed = shared_state.clone();
            tokio::spawn(async move {
                while let Some(msg) = sub_feed.recv().await {
                    let mut st = shared_state_feed.lock().await;
                    match msg {
                        SubaccountsMessage::Initial(init) => {
                            st.last_snapshot = Some(format!("{:#?}", init.contents));
                        }
                        SubaccountsMessage::Update(upd) => {
                            st.last_update = Some(format!("{:#?}", upd.contents));
                        }
                    }
                    let _ = acct_tx_feed.send(st.clone());
                }
            });

            // Command loop: updates last_tx / last_error, logs trades
            let shared_state_cmd = shared_state.clone();
            let mut trade_rx = trade_rx;
            while let Some(cmd) = trade_rx.recv().await {
                let mut st = shared_state_cmd.lock().await;

                let (ticker_str, side, size) = match cmd {
                    TradeCmd::MarketBuy { ticker, size } => (ticker, OrderSide::Buy, size),
                    TradeCmd::MarketSell { ticker, size } => (ticker, OrderSide::Sell, size),
                };

                let market = match indexer
                    .markets()
                    .get_perpetual_market(&ticker_str.clone().into())
                    .await
                {
                    Ok(m) => m,
                    Err(e) => {
                        st.last_error = Some(format!("market meta error: {e}"));
                        let _ = acct_tx.send(st.clone());
                        continue;
                    }
                };

                let h = match node.latest_block_height().await {
                    Ok(h) => h,
                    Err(e) => {
                        st.last_error = Some(format!("height error: {e}"));
                        let _ = acct_tx.send(st.clone());
                        continue;
                    }
                };

                // derive fresh subaccount for each order to avoid move issues
                let sub_for_orders = match account.subaccount(0) {
                    Ok(s) => s,
                    Err(e) => {
                        st.last_error = Some(format!("subaccount derive error: {e}"));
                        let _ = acct_tx.send(st.clone());
                        continue;
                    }
                };

                let (_id, order) = match OrderBuilder::new(market, sub_for_orders)
                    .market(side, size.clone())
                    .reduce_only(false)
                    .price(100)
                    .time_in_force(TimeInForce::Unspecified)
                    .until(h.ahead(10))
                    .build(123456)
                {
                    Ok(x) => x,
                    Err(e) => {
                        st.last_error = Some(format!("build order error: {e}"));
                        let _ = acct_tx.send(st.clone());
                        continue;
                    }
                };

                match node.place_order(&mut account, order).await {
                    Ok(tx_hash) => {
                        // log "resolved trade" event
                        let ts = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap()
                            .as_secs();
                        let side_str = match side {
                            OrderSide::Buy => "buy",
                            OrderSide::Sell => "sell",
                            _ => "unspecified",
                        };
                        let _ = writeln!(
                            trades_file,
                            "{ts},{:?},{},{side_str},{}",
                            tx_hash,
                            ticker_str,
                            size
                        );
                        let _ = trades_file.flush();

                        st.last_tx = Some(format!("{tx_hash:?}"));
                        st.last_error = None;
                    }
                    Err(e) => {
                        st.last_error = Some(format!("place_order error: {e}"));
                    }
                }

                let _ = acct_tx.send(st.clone());
            }
        }
    });

    let options = eframe::NativeOptions::default();
    if let Err(e) = eframe::run_native(
        "Ladder GUI (egui)",
        options,
        Box::new(|_cc| Box::new(MyApp::new(book_rx, trade_tx, acct_rx))),
    ) {
        eprintln!("eframe error: {e}");
    }

    drop(rt);
}
