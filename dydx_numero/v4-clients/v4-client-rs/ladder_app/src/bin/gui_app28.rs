// ladder_app/src/bin/gui_app28.rs
//
// Egui desktop app with live dYdX v4 testnet orderbook.
// Uses:
//   eframe/egui 0.27
//   egui_plot 0.27
//
// Features:
//  - 30s / 1m / 3m / 5m candle aggregation (real bucket timing)
//  - Price driven by real testnet mid if available, else random walk fallback
//  - Candles flush within TF (time-based width)
//  - Last candle centered by default, shared horizontal pan across TFs
//  - Horizontal zoom + pan controls
//  - RSI plot under candles
//  - Volume bars under candles
//  - LIVE orderbook + depth (dYdX testnet)
//  - Fake trading panel (margin/lev/pos/TP/SL + liquidation)
//  - REAL testnet market BUY/SELL via DYDX_TESTNET_MNEMONIC
//  - Orderbook + trades logging to CSV for later replay
//  - Ticker dropdown: ETH-USD / BTC-USD / SOL-USD (book + trading + logs)
//  - Theme selector (5 palettes)
//  - Time display toggle: UNIX / local time
//  - Scrollable main page so trading panel always reachable
//
// Run:
//   export DYDX_TESTNET_MNEMONIC='...'
//   cargo run -p ladder_app --bin gui_app28

mod candle_agg;

use candle_agg::{Candle, CandleAgg};

use eframe::egui;
use egui::{Color32, RichText};
use egui_plot::{GridMark, HLine, Line, Plot, PlotBounds, PlotPoints, VLine};

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use std::collections::BTreeMap;
use std::env;
use std::fs::OpenOptions;
use std::io::Write;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::runtime::Runtime;
use tokio::sync::{mpsc, watch};

use bigdecimal::BigDecimal;
use bigdecimal::Zero;

use chrono::{Local, TimeZone};

use dydx_client::config::ClientConfig;
use dydx_client::indexer::{
    Feed as DxFeed, Feeds, IndexerClient, OrdersMessage, OrderbookResponsePriceLevel, Price,
    Quantity, Ticker,
};
use dydx_client::node::{NodeClient, OrderBuilder, OrderSide, Wallet};
use dydx_proto::dydxprotocol::clob::order::TimeInForce;

// ---- rustls crypto provider ----

fn init_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

// ---- helpers ----

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn bd_to_f64(bd: &bigdecimal::BigDecimal) -> f64 {
    bd.to_string().parse::<f64>().unwrap_or(0.0)
}

fn price_to_f64(p: &Price) -> f64 {
    bd_to_f64(&p.0)
}

fn qty_to_f64(q: &Quantity) -> f64 {
    bd_to_f64(&q.0)
}

fn append_line(path: &str, line: &str) {
    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{line}");
    }
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

    pub fn mid(&self) -> Option<f64> {
        if let (Some((bp, _)), Some((ap, _))) =
            (self.bids.iter().next_back(), self.asks.iter().next())
        {
            let bid = price_to_f64(bp);
            let ask = price_to_f64(ap);
            if bid > 0.0 && ask > 0.0 {
                return Some((bid + ask) * 0.5);
            }
        }
        None
    }
}

// ---- chart settings ----

#[derive(Clone, Copy)]
struct ChartSettings {
    y_min: f64,
    y_max: f64,
    show_candles: usize,
    auto_scale: bool,
    // shared across TFs
    x_zoom: f64,     // >1 zoom-in, <1 zoom-out
    x_pan_secs: f64, // pan along time axis
}

// ---- display time mode ----

#[derive(Clone, Copy, PartialEq, Eq)]
enum TimeDisplayMode {
    Unix,
    LocalTime,
}

fn format_ts_common(mode: TimeDisplayMode, value: f64) -> String {
    let ts = value as i64;
    match mode {
        TimeDisplayMode::Unix => format!("{ts}"),
        TimeDisplayMode::LocalTime => {
            let dt = Local
                .timestamp_opt(ts, 0)
                .single()
                .unwrap_or_else(|| Local.timestamp_opt(0, 0).single().unwrap());
            dt.format("%H:%M:%S").to_string()
        }
    }
}

// ---- UI theme ----

#[derive(Clone, Copy, PartialEq, Eq)]
enum ThemeId {
    Dark,
    Light,
    Ocean,
    Sunset,
    Matrix,
}

#[derive(Clone)]
struct Theme {
    bg: Color32,
    panel_bg: Color32,
    text: Color32,
    accent: Color32,
    long: Color32,
    short: Color32,
    grid: Color32,
}

fn theme_by_id(id: ThemeId) -> Theme {
    match id {
        ThemeId::Dark => Theme {
            bg: Color32::from_rgb(10, 10, 20),
            panel_bg: Color32::from_rgb(20, 20, 35),
            text: Color32::from_rgb(230, 230, 240),
            accent: Color32::from_rgb(120, 170, 255),
            long: Color32::from_rgb(80, 200, 120),
            short: Color32::from_rgb(240, 90, 90),
            grid: Color32::from_rgb(60, 60, 80),
        },
        ThemeId::Light => Theme {
            bg: Color32::from_rgb(245, 245, 250),
            panel_bg: Color32::from_rgb(235, 235, 245),
            text: Color32::from_rgb(20, 20, 30),
            accent: Color32::from_rgb(60, 120, 220),
            long: Color32::from_rgb(40, 160, 80),
            short: Color32::from_rgb(200, 60, 60),
            grid: Color32::from_rgb(200, 200, 220),
        },
        ThemeId::Ocean => Theme {
            bg: Color32::from_rgb(4, 18, 36),
            panel_bg: Color32::from_rgb(8, 30, 52),
            text: Color32::from_rgb(210, 230, 250),
            accent: Color32::from_rgb(90, 180, 255),
            long: Color32::from_rgb(80, 220, 180),
            short: Color32::from_rgb(255, 120, 140),
            grid: Color32::from_rgb(40, 80, 120),
        },
        ThemeId::Sunset => Theme {
            bg: Color32::from_rgb(20, 10, 25),
            panel_bg: Color32::from_rgb(40, 20, 55),
            text: Color32::from_rgb(250, 230, 230),
            accent: Color32::from_rgb(255, 170, 90),
            long: Color32::from_rgb(255, 120, 80),
            short: Color32::from_rgb(150, 120, 255),
            grid: Color32::from_rgb(80, 50, 90),
        },
        ThemeId::Matrix => Theme {
            bg: Color32::from_rgb(2, 8, 2),
            panel_bg: Color32::from_rgb(5, 15, 5),
            text: Color32::from_rgb(170, 255, 170),
            accent: Color32::from_rgb(90, 255, 120),
            long: Color32::from_rgb(120, 255, 160),
            short: Color32::from_rgb(255, 80, 80),
            grid: Color32::from_rgb(20, 60, 20),
        },
    }
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

    // position (fake engine)
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

    // REAL testnet trade inputs
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

// ---- app ----

struct MyApp {
    // core
    selected_tab: Tab,
    last_price: f64,
    last_volume: f64,

    tf_30s: CandleAgg,
    tf_1m: CandleAgg,
    tf_3m: CandleAgg,
    tf_5m: CandleAgg,
    selected_tf: u64, // in seconds

    chart: ChartSettings,
    time_display: TimeDisplayMode,
    current_theme_id: ThemeId,

    // ticker
    available_tickers: Vec<&'static str>,
    current_ticker: String,
    show_ticker_popup: bool,

    // fake trading
    trading: TradingState,

    // live book
    live_book_rx: watch::Receiver<LiveBook>,
    live_book: LiveBook,

    // real trading
    trade_tx: mpsc::Sender<TradeCmd>,
    acct_rx: watch::Receiver<AccountState>,
    acct_state: AccountState,
    ticker_tx: watch::Sender<String>,

    rng: StdRng,
}

impl MyApp {
    fn new(
        live_book_rx: watch::Receiver<LiveBook>,
        trade_tx: mpsc::Sender<TradeCmd>,
        acct_rx: watch::Receiver<AccountState>,
        ticker_tx: watch::Sender<String>,
    ) -> Self {
        Self {
            selected_tab: Tab::Candles,
            last_price: 3000.0,
            last_volume: 0.0,

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
                x_zoom: 1.0,
                x_pan_secs: 0.0,
            },
            time_display: TimeDisplayMode::LocalTime,
            current_theme_id: ThemeId::Dark,

            available_tickers: vec!["ETH-USD", "BTC-USD", "SOL-USD"],
            current_ticker: "ETH-USD".to_string(),
            show_ticker_popup: false,

            trading: TradingState::new(),

            live_book: LiveBook::default(),
            live_book_rx,

            trade_tx,
            acct_rx,
            acct_state: AccountState::default(),
            ticker_tx,

            rng: StdRng::seed_from_u64(42),
        }
    }

    fn theme(&self) -> Theme {
        theme_by_id(self.current_theme_id)
    }

    fn current_series(&self) -> Vec<Candle> {
        match self.selected_tf {
            30 => self.tf_30s.get_series(),
            60 => self.tf_1m.get_series(),
            180 => self.tf_3m.get_series(),
            300 => self.tf_5m.get_series(),
            _ => self.tf_1m.get_series(),
        }
    }

    fn tick(&mut self) {
        let ts = now_secs();

        // update live book
        if self.live_book_rx.has_changed().unwrap_or(false) {
            self.live_book = self.live_book_rx.borrow().clone();
        }

        // price:
        let mut have_live_mid = false;
        if let Some(mid) = self.live_book.mid() {
            self.last_price = mid;
            have_live_mid = true;
        }

        if !have_live_mid {
            let step: f64 = self.rng.random_range(-2.0..2.0);
            self.last_price = (self.last_price + step).clamp(2950.0, 3050.0);
        }

        // approximate volume from top-of-book depth
        self.last_volume = if let (Some((_, bs)), Some((_, asz))) =
            (self.live_book.bids.iter().next_back(), self.live_book.asks.iter().next())
        {
            (qty_to_f64(bs) + qty_to_f64(asz)).max(0.0)
        } else {
            0.0
        };

        // account state updates from real trading
        if self.acct_rx.has_changed().unwrap_or(false) {
            self.acct_state = self.acct_rx.borrow().clone();
        }

        // fake engine checks
        self.trading.check_tp_sl(self.last_price);
        self.trading.check_liquidation(self.last_price, ts);

        // update candle aggs
        self.tf_30s.update(ts, self.last_price, self.last_volume);
        self.tf_1m.update(ts, self.last_price, self.last_volume);
        self.tf_3m.update(ts, self.last_price, self.last_volume);
        self.tf_5m.update(ts, self.last_price, self.last_volume);
    }

    // ---- UI ----

    fn ui_top_bar(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme();

        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.selected_tab, Tab::Orderbook, "Orderbook + Depth");
            ui.selectable_value(&mut self.selected_tab, Tab::Candles, "Candles + RSI + Vol");
            ui.separator();

            ui.label("TF:");
            if ui.button("30s").clicked() {
                self.selected_tf = 30;
            }
            if ui.button("1m").clicked() {
                self.selected_tf = 60;
            }
            if ui.button("3m").clicked() {
                self.selected_tf = 180;
            }
            if ui.button("5m").clicked() {
                self.selected_tf = 300;
            }

            ui.separator();
            ui.checkbox(&mut self.chart.auto_scale, "Auto-scale Y");

            ui.separator();
            ui.label(format!("Mark: {:.2}", self.last_price));

            ui.separator();
            ui.menu_button("Theme", |ui| {
                let ids = [
                    (ThemeId::Dark, "Dark"),
                    (ThemeId::Light, "Light"),
                    (ThemeId::Ocean, "Ocean"),
                    (ThemeId::Sunset, "Sunset"),
                    (ThemeId::Matrix, "Matrix"),
                ];
                for (id, label) in ids {
                    if ui
                        .selectable_label(self.current_theme_id == id, label)
                        .clicked()
                    {
                        self.current_theme_id = id;
                        ui.close_menu();
                    }
                }
            });

            ui.separator();
            ui.menu_button("Time", |ui| {
                if ui
                    .selectable_label(
                        self.time_display == TimeDisplayMode::Unix,
                        "UNIX seconds",
                    )
                    .clicked()
                {
                    self.time_display = TimeDisplayMode::Unix;
                    ui.close_menu();
                }
                if ui
                    .selectable_label(
                        self.time_display == TimeDisplayMode::LocalTime,
                        "Local time",
                    )
                    .clicked()
                {
                    self.time_display = TimeDisplayMode::LocalTime;
                    ui.close_menu();
                }
            });

            ui.separator();
            ui.label(RichText::new(&self.current_ticker).color(theme.accent));

            if ui.button("Change Ticker").clicked() {
                self.show_ticker_popup = true;
            }
        });

        if self.show_ticker_popup {
            egui::Window::new("Select Ticker")
                .collapsible(false)
                .resizable(false)
                .show(ui.ctx(), |ui| {
                    for t in &self.available_tickers {
                        if ui
                            .selectable_label(self.current_ticker == *t, *t)
                            .clicked()
                        {
                            self.current_ticker = (*t).to_string();
                            let _ = self.ticker_tx.send(self.current_ticker.clone());
                            self.chart.x_pan_secs = 0.0;
                        }
                    }
                    if ui.button("Close").clicked() {
                        self.show_ticker_popup = false;
                    }
                });
        }
    }

    fn ui_trading_panel(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme();

        ui.group(|ui| {
            ui.heading("Balances + Trade (fake + real)");

            if self.trading.liquidated_flag {
                ui.colored_label(theme.short, "⚠ LIQUIDATED");
                if let (Some(px), Some(t)) =
                    (self.trading.last_liq_price, self.trading.last_liq_time)
                {
                    ui.label(format!("Liquidated @ {:.2} (t={})", px, t));
                }
                ui.separator();
            }

            ui.label(format!("Wallet USDC: {:.2}", self.trading.wallet_usdc));
            ui.label(format!("Margin USDC: {:.2}", self.trading.margin));
            ui.separator();

            // transfers
            ui.horizontal(|ui| {
                ui.label("Deposit:");
                ui.add(
                    egui::DragValue::new(&mut self.trading.deposit_amount)
                        .speed(1.0)
                        .clamp_range(0.0..=1_000_000.0),
                );
                if ui.button("Wallet → Margin").clicked() {
                    let amt = self.trading.deposit_amount;
                    self.trading.deposit_to_margin(amt);
                }
            });
            ui.horizontal(|ui| {
                ui.label("Withdraw:");
                ui.add(
                    egui::DragValue::new(&mut self.trading.withdraw_amount)
                        .speed(1.0)
                        .clamp_range(0.0..=1_000_000.0),
                );
                if ui.button("Margin → Wallet").clicked() {
                    let amt = self.trading.withdraw_amount;
                    self.trading.withdraw_from_margin(amt);
                }
            });

            ui.separator();
            ui.label(format!("Mark: {:.2}", self.last_price));
            ui.separator();

            // side + leverage + position
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
                if ui.button("Open / Close").clicked() {
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
            ui.heading("REAL testnet order (market)");

            ui.horizontal(|ui| {
                ui.label("Ticker:");
                ui.label(&self.current_ticker);
                ui.label("Size (units):");
                ui.add(
                    egui::DragValue::new(&mut self.trading.real_size_units)
                        .speed(0.001)
                        .clamp_range(0.0..=1000.0),
                );
            });

            ui.horizontal(|ui| {
                if ui.button("Market BUY").clicked() {
                    let t = self.current_ticker.trim().to_string();
                    let s_str = format!("{:.8}", self.trading.real_size_units.max(0.0));
                    if let Some(size) = BigDecimal::parse_bytes(s_str.as_bytes(), 10) {
                        let _ = self
                            .trade_tx
                            .try_send(TradeCmd::MarketBuy { ticker: t, size });
                    }
                }
                if ui.button("Market SELL").clicked() {
                    let t = self.current_ticker.trim().to_string();
                    let s_str = format!("{:.8}", self.trading.real_size_units.max(0.0));
                    if let Some(size) = BigDecimal::parse_bytes(s_str.as_bytes(), 10) {
                        let _ = self
                            .trade_tx
                            .try_send(TradeCmd::MarketSell { ticker: t, size });
                    }
                }
            });

            if let Some(tx) = &self.acct_state.last_tx {
                ui.label(format!("Last tx: {tx}"));
            }
            if let Some(err) = &self.acct_state.last_error {
                ui.colored_label(theme.short, format!("Last error: {err}"));
            }
        });
    }

    fn ui_orderbook(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme();

        let avail_h = ui.available_height();
        let avail_w = ui.available_width();

        ui.heading(format!(
            "Orderbook + Depth (LIVE testnet)  | {}",
            self.current_ticker
        ));

        ui.separator();

        ui.allocate_ui(egui::vec2(avail_w, avail_h), |ui| {
            ui.horizontal(|ui| {
                let left_w = avail_w * 0.45;
                let right_w = avail_w * 0.55;

                // depth plot
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
                        .show(ui, |plot_ui| {
                            if !bid_points.is_empty() {
                                let pts: PlotPoints = bid_points
                                    .iter()
                                    .map(|(x, y)| [*x, *y])
                                    .collect::<Vec<_>>()
                                    .into();
                                plot_ui.line(Line::new(pts).color(theme.long).name("Bids"));
                            }
                            if !ask_points.is_empty() {
                                let pts: PlotPoints = ask_points
                                    .iter()
                                    .map(|(x, y)| [*x, *y])
                                    .collect::<Vec<_>>()
                                    .into();
                                plot_ui.line(Line::new(pts).color(theme.short).name("Asks"));
                            }
                        });
                });

                ui.separator();

                // ladders + trading panel
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
                                for (p, s) in self.live_book.bids.iter().rev().take(20) {
                                    ui.label(format!("{:>10.2}", price_to_f64(p)));
                                    ui.label(format!("{:>8.4}", qty_to_f64(s)));
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
                                for (p, s) in self.live_book.asks.iter().take(20) {
                                    ui.label(format!("{:>10.2}", price_to_f64(p)));
                                    ui.label(format!("{:>8.4}", qty_to_f64(s)));
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
        let theme = self.theme();

        let series_vec = self.current_series();
        if series_vec.is_empty() {
            ui.label("No candles yet. Wait a few seconds for data.");
            return;
        }

        // top controls
        ui.horizontal(|ui| {
            ui.label("History (candles):");
            ui.add(
                egui::Slider::new(&mut self.chart.show_candles, 20..=600)
                    .logarithmic(true),
            );

            ui.separator();
            ui.label("X Zoom:");
            ui.add(
                egui::Slider::new(&mut self.chart.x_zoom, 0.25..=4.0)
                    .logarithmic(true)
                    .text("zoom"),
            );

            ui.horizontal(|ui| {
                if ui.button("← Pan").clicked() {
                    self.chart.x_pan_secs -= self.selected_tf as f64 * 10.0;
                }
                if ui.button("Pan →").clicked() {
                    self.chart.x_pan_secs += self.selected_tf as f64 * 10.0;
                }
                if ui.button("Center").clicked() {
                    self.chart.x_pan_secs = 0.0;
                }
            });

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

        let avail_h = ui.available_height();
        let avail_w = ui.available_width();
        let candles_h = avail_h * 0.5;
        let vol_h = avail_h * 0.18;
        let rsi_h = avail_h * 0.18;
        let bottom_h = avail_h * 0.14;

        // ---- candles plot ----
        ui.allocate_ui(egui::vec2(avail_w, candles_h), |ui| {
            Plot::new("candles_plot")
                .height(candles_h)
                .include_y(y_min)
                .include_y(y_max)
                .x_axis_formatter({
                    let mode = self.time_display;
                    move |mark: GridMark, _bounds, _transform| {
                        format_ts_common(mode, mark.value)
                    }
                })
                .show(ui, |plot_ui| {
                    let tf = self.selected_tf as f64;

                    let last = visible.last().unwrap();
                    let t_center = last.t as f64 + tf; // end of current candle
                    let base_span = tf * self.chart.show_candles as f64;
                    let span = base_span / self.chart.x_zoom.max(1e-6);

                    let x_max = t_center + span / 2.0 + self.chart.x_pan_secs;
                    let x_min = t_center - span / 2.0 + self.chart.x_pan_secs;

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
                            theme.long
                        } else {
                            theme.short
                        };

                        // wick
                        let wick_pts: PlotPoints = vec![[mid, c.low], [mid, c.high]].into();
                        plot_ui.line(Line::new(wick_pts).color(color));

                        // full body (rectangle edges)
                        let left_edge: PlotPoints = vec![[left, bot], [left, top]].into();
                        let right_edge: PlotPoints = vec![[right, bot], [right, top]].into();
                        let top_edge: PlotPoints = vec![[left, top], [right, top]].into();
                        let bot_edge: PlotPoints = vec![[left, bot], [right, bot]].into();

                        plot_ui.line(Line::new(left_edge).color(color).width(2.0));
                        plot_ui.line(Line::new(right_edge).color(color).width(2.0));
                        plot_ui.line(Line::new(top_edge).color(color).width(2.0));
                        plot_ui.line(Line::new(bot_edge).color(color).width(2.0));
                    }

                    // rulers
                    let now_x = last.t as f64 + tf;
                    let now_px = last.close;
                    plot_ui.hline(HLine::new(now_px).color(theme.accent).name("now_px"));
                    plot_ui.vline(VLine::new(now_x).color(theme.accent).name("now_t"));

                    // position lines
                    if let Some(entry) = self.trading.entry_price {
                        plot_ui.hline(HLine::new(entry).color(theme.long).name("entry"));
                    }
                    if let Some(tp) = self.trading.take_profit {
                        plot_ui.hline(HLine::new(tp).color(theme.accent).name("TP"));
                    }
                    if let Some(sl) = self.trading.stop_loss {
                        plot_ui.hline(HLine::new(sl).color(theme.short).name("SL"));
                    }
                    if let Some(liq_px) = self.trading.last_liq_price {
                        plot_ui.hline(HLine::new(liq_px).color(theme.short).name("LIQ"));
                    }
                });
        });

        ui.separator();

        // ---- volume bars ----
        ui.allocate_ui(egui::vec2(avail_w, vol_h), |ui| {
            Plot::new("vol_plot")
                .height(vol_h)
                .include_y(0.0)
                .include_y(1.0)
                .x_axis_formatter({
                    let mode = self.time_display;
                    move |mark: GridMark, _bounds, _transform| {
                        format_ts_common(mode, mark.value)
                    }
                })
                .show(ui, |plot_ui| {
                    let tf = self.selected_tf as f64;
                    let last = visible.last().unwrap();
                    let t_center = last.t as f64 + tf;
                    let base_span = tf * self.chart.show_candles as f64;
                    let span = base_span / self.chart.x_zoom.max(1e-6);
                    let x_max = t_center + span / 2.0 + self.chart.x_pan_secs;
                    let x_min = t_center - span / 2.0 + self.chart.x_pan_secs;

                    let max_vol = visible
                        .iter()
                        .map(|c| c.volume)
                        .fold(0.0f64, f64::max)
                        .max(1e-6);

                    plot_ui.set_plot_bounds(PlotBounds::from_min_max([x_min, 0.0], [x_max, 1.0]));

                    for c in visible {
                        let left = c.t as f64;
                        let right = left + tf;
                        let mid = left + tf * 0.5;
                        let v = (c.volume / max_vol).clamp(0.0, 1.0);

                        let color = theme.accent;

                        let pts: PlotPoints = vec![[mid, 0.0], [mid, v]].into();
                        plot_ui.line(Line::new(pts).color(color).width(3.0));

                        let left_edge: PlotPoints = vec![[left, 0.0], [left, v]].into();
                        let right_edge: PlotPoints = vec![[right, 0.0], [right, v]].into();
                        plot_ui.line(Line::new(left_edge).color(color));
                        plot_ui.line(Line::new(right_edge).color(color));
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

            Plot::new("rsi_plot")
                .height(rsi_h)
                .include_y(0.0)
                .include_y(100.0)
                .x_axis_formatter({
                    let mode = self.time_display;
                    move |mark: GridMark, _bounds, _transform| {
                        format_ts_common(mode, mark.value)
                    }
                })
                .show(ui, |plot_ui| {
                    let tf = self.selected_tf as f64;
                    let last = visible.last().unwrap();
                    let t_center = last.t as f64 + tf;
                    let base_span = tf * self.chart.show_candles as f64;
                    let span = base_span / self.chart.x_zoom.max(1e-6);
                    let x_max = t_center + span / 2.0 + self.chart.x_pan_secs;
                    let x_min = t_center - span / 2.0 + self.chart.x_pan_secs;

                    plot_ui.set_plot_bounds(PlotBounds::from_min_max(
                        [x_min, 0.0],
                        [x_max, 100.0],
                    ));

                    if !rsi_visible.is_empty() {
                        let pts: PlotPoints = rsi_visible
                            .iter()
                            .map(|(t, v)| [*t, *v])
                            .collect::<Vec<_>>()
                            .into();
                        plot_ui.line(Line::new(pts).color(theme.accent).name("RSI"));
                        plot_ui.hline(HLine::new(70.0).color(theme.short));
                        plot_ui.hline(HLine::new(30.0).color(theme.long));
                    }
                });
        });

        ui.separator();

        // ---- bottom info + trading ----
        ui.allocate_ui(egui::vec2(avail_w, bottom_h), |ui| {
            ui.columns(2, |cols| {
                cols[0].group(|ui| {
                    ui.label(format!("Ticker: {}", self.current_ticker));
                    ui.label("Last candle:");
                    if let Some(c) = series_vec.last() {
                        ui.label(format!("t_start unix: {}", c.t));
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
}

impl eframe::App for MyApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let theme = self.theme();

        // apply background color
        ctx.style_mut(|s| {
            s.visuals.window_fill = theme.panel_bg;
            s.visuals.panel_fill = theme.panel_bg;
            s.visuals.extreme_bg_color = theme.bg;
            s.visuals.override_text_color = Some(theme.text);
        });

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

// ---- backend tasks ----

async fn run_orderbook_task(
    book_tx: watch::Sender<LiveBook>,
    mut ticker_rx: watch::Receiver<String>,
) -> anyhow::Result<()> {
    let config = ClientConfig::from_file("client/tests/testnet.toml").await?;
    let mut current_ticker = ticker_rx.borrow().clone();

    loop {
        let mut indexer = IndexerClient::new(config.indexer.clone());
        let mut feeds: Feeds<'_> = indexer.feed();

        let ticker = Ticker(current_ticker.clone());
        let mut feed: DxFeed<OrdersMessage> = match feeds.orders(&ticker, false).await {
            Ok(f) => f,
            Err(e) => {
                eprintln!("orders feed failed for {}: {e}", current_ticker);
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };

        let mut book = LiveBook::default();

        loop {
            tokio::select! {
                msg_opt = feed.recv() => {
                    let Some(msg) = msg_opt else {
                        eprintln!("orders feed closed for {}", current_ticker);
                        break;
                    };

                    let ts = now_secs(); // we use our own timestamp

                    match msg {
                        OrdersMessage::Initial(init) => {
                            let bids = init.contents.bids;
                            let asks = init.contents.asks;

                            // update book
                            book.apply_initial(bids.clone(), asks.clone());

                            // log each level
                            let path = format!("data/orderbook_{}.csv", current_ticker.replace('/', "_"));
                            for lvl in bids {
                                let p = price_to_f64(&lvl.price);
                                let s = qty_to_f64(&lvl.size);
                                append_line(
                                    &path,
                                    &format!("{ts},\"{}\",book,bid,{p:.8},{s:.8}", current_ticker),
                                );
                            }
                            for lvl in asks {
                                let p = price_to_f64(&lvl.price);
                                let s = qty_to_f64(&lvl.size);
                                append_line(
                                    &path,
                                    &format!("{ts},\"{}\",book,ask,{p:.8},{s:.8}", current_ticker),
                                );
                            }
                        }
                        OrdersMessage::Update(upd) => {
                            let bids = upd.contents.bids.clone();
                            let asks = upd.contents.asks.clone();

                            book.apply_update(bids.clone(), asks.clone());

                            let path = format!("data/orderbook_{}.csv", current_ticker.replace('/', "_"));

                            if let Some(b) = bids {
                                for lvl in b {
                                    let p = price_to_f64(&lvl.price);
                                    let s = qty_to_f64(&lvl.size);
                                    append_line(
                                        &path,
                                        &format!("{ts},\"{}\",book_delta,bid,{p:.8},{s:.8}", current_ticker),
                                    );
                                }
                            }
                            if let Some(a) = asks {
                                for lvl in a {
                                    let p = price_to_f64(&lvl.price);
                                    let s = qty_to_f64(&lvl.size);
                                    append_line(
                                        &path,
                                        &format!("{ts},\"{}\",book_delta,ask,{p:.8},{s:.8}", current_ticker),
                                    );
                                }
                            }
                        }
                    }

                    let _ = book_tx.send(book.clone());
                }

                changed = ticker_rx.changed() => {
                    if changed.is_ok() {
                        let new_ticker = ticker_rx.borrow().clone();
                        if new_ticker != current_ticker {
                            eprintln!("orderbook: switching ticker {} -> {}", current_ticker, new_ticker);
                            current_ticker = new_ticker;
                            break; // break inner loop to resubscribe
                        }
                    } else {
                        break;
                    }
                }
            }
        }
    }
}

async fn run_trading_task(
    mut trade_rx: mpsc::Receiver<TradeCmd>,
    acct_tx: watch::Sender<AccountState>,
    _ticker_rx: watch::Receiver<String>,
) -> anyhow::Result<()> {
    let config = ClientConfig::from_file("client/tests/testnet.toml").await?;

    let raw = match env::var("DYDX_TESTNET_MNEMONIC") {
        Ok(v) => v,
        Err(_) => {
            eprintln!("REAL trading disabled: DYDX_TESTNET_MNEMONIC not set");
            return Ok(());
        }
    };
    let mnemonic = raw.split_whitespace().collect::<Vec<_>>().join(" ");

    let wallet = Wallet::from_mnemonic(&mnemonic)?;

    let mut node = NodeClient::connect(config.node.clone()).await?;
    let mut account = wallet.account(0, &mut node).await?;

    let indexer = IndexerClient::new(config.indexer.clone());

    let mut local_acct_state = AccountState::default();

    while let Some(cmd) = trade_rx.recv().await {
        let (ticker_str, side, size) = match cmd {
            TradeCmd::MarketBuy { ticker, size } => (ticker, OrderSide::Buy, size),
            TradeCmd::MarketSell { ticker, size } => (ticker, OrderSide::Sell, size),
        };

        // fresh subaccount each loop to avoid move issues
        let sub = account.subaccount(0)?;

        let market = match indexer
            .markets()
            .get_perpetual_market(&ticker_str.clone().into())
            .await
        {
            Ok(m) => m,
            Err(e) => {
                local_acct_state.last_error =
                    Some(format!("market meta error for {}: {e}", ticker_str));
                let _ = acct_tx.send(local_acct_state.clone());
                continue;
            }
        };

        let h = match node.latest_block_height().await {
            Ok(h) => h,
            Err(e) => {
                local_acct_state.last_error = Some(format!("height error: {e}"));
                let _ = acct_tx.send(local_acct_state.clone());
                continue;
            }
        };

        let (_id, order) = match OrderBuilder::new(market, sub)
            .market(side, size.clone())
            .reduce_only(false)
            .price(100)
            .time_in_force(TimeInForce::Unspecified)
            .until(h.ahead(10))
            .build(123456)
        {
            Ok(x) => x,
            Err(e) => {
                local_acct_state.last_error = Some(format!("build order error: {e}"));
                let _ = acct_tx.send(local_acct_state.clone());
                continue;
            }
        };

        match node.place_order(&mut account, order).await {
            Ok(tx_hash) => {
                let tx_str = format!("{tx_hash:?}");
                local_acct_state.last_tx = Some(tx_str.clone());
                local_acct_state.last_error = None;

                // log trade
                let side_str = match side {
                    OrderSide::Buy => "buy",
                    OrderSide::Sell => "sell",
                    OrderSide::Unspecified => "unspecified",
                };
                let ts = now_secs();
                let path = format!("data/trades_{}.csv", ticker_str.replace('/', "_"));
                append_line(
                    &path,
                    &format!("{ts},\"{}\",real,{side_str},{:?}", ticker_str, size),
                );
            }
            Err(e) => {
                local_acct_state.last_error = Some(format!("place_order error: {e}"));
            }
        }

        let _ = acct_tx.send(local_acct_state.clone());
    }

    Ok(())
}

// ---- main ----

fn main() {
    init_crypto_provider();

    // channels
    let (book_tx, book_rx) = watch::channel(LiveBook::default());
    let (ticker_tx, ticker_rx) = watch::channel::<String>("ETH-USD".to_string());
    let (trade_tx, trade_rx) = mpsc::channel::<TradeCmd>(32);
    let (acct_tx, acct_rx) = watch::channel::<AccountState>(AccountState::default());

    // tokio runtime
    let rt = Runtime::new().expect("tokio runtime");

    // orderbook task
    {
        let book_tx = book_tx.clone();
        let ticker_rx = ticker_rx.clone();
        rt.spawn(async move {
            if let Err(e) = run_orderbook_task(book_tx, ticker_rx).await {
                eprintln!("orderbook task error: {e}");
            }
        });
    }

    // trading task
    {
        let trade_rx = trade_rx;
        let acct_tx = acct_tx.clone();
        let ticker_rx = ticker_rx.clone();
        rt.spawn(async move {
            if let Err(e) = run_trading_task(trade_rx, acct_tx, ticker_rx).await {
                eprintln!("trading task error: {e}");
            }
        });
    }

    let native_options = eframe::NativeOptions::default();
    if let Err(e) = eframe::run_native(
        "Ladder GUI (dYdX testnet)",
        native_options,
        Box::new(|_cc| {
            Box::new(MyApp::new(
                book_rx,
                trade_tx,
                acct_rx,
                ticker_tx,
            ))
        }),
    ) {
        eprintln!("eframe error: {e}");
    }

    drop(rt);
}
