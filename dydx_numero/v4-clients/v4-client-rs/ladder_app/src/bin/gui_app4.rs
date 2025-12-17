// ladder_app/src/bin/gui_app2.rs
//
// Egui desktop app equivalent of the TUI trading sim.
// Works with:
//   eframe/egui 0.27
//   egui_plot 0.27
//
// Features:
//  - 30s / 1m / 3m / 5m candle aggregation (real bucket timing)
//  - Random-walk price
//  - Candles on real-time x-axis, flush within TF
//  - Horizontal stretch/squeeze (x_zoom) + pan (x_pan_secs)
//  - RSI plot under candles (real-time x)
//  - Fake orderbook + depth tab
//  - Fake trading panel (margin/lev/pos/TP/SL)
//  - NEW: USDC wallet + deposit/withdraw to margin
//  - Panels flex proportionally to window size
//
// Run:
//   cargo run -p ladder_app --bin gui_app2

mod candle_agg;

use candle_agg::{Candle, CandleAgg};
use eframe::egui;
use egui::Color32;
use egui_plot::{HLine, Line, Plot, PlotBounds, PlotPoints, VLine};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Clone)]
struct ChartSettings {
    y_min: f64,
    y_max: f64,
    show_candles: usize,
    auto_scale: bool,

    // horizontal zoom/pan
    x_zoom: f64,     // >1 zoom-in (stretch), <1 zoom-out (squeeze)
    x_pan_secs: f64, // pan in seconds (positive moves right, negative left)
}

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
    // --- balances ---
    wallet_usdc: f64,   // off-margin USDC wallet
    margin: f64,        // margin account USDC

    // UI inputs for transfers
    deposit_amount: f64,
    withdraw_amount: f64,

    // --- position ---
    leverage: f64,
    position: f64,
    side: PositionSide,
    entry_price: Option<f64>,
    realized_pnl: f64,
    take_profit: Option<f64>,
    stop_loss: Option<f64>,
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
        }
    }

    // wallet -> margin
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

    // margin -> wallet
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

    fn open_at(&mut self, mark: f64) {
        if self.is_open() || self.side == PositionSide::Flat {
            return;
        }
        if self.margin <= 0.0 || self.leverage <= 0.0 || mark <= 0.0 {
            return;
        }

        if self.position <= 0.0 {
            let notional = self.notional();
            self.position = notional / mark;
        }

        self.entry_price = Some(mark);
    }

    fn close_at(&mut self, mark: f64) {
        if !self.is_open() {
            return;
        }

        let upnl = self.unrealized_pnl(mark);
        self.realized_pnl += upnl;

        self.position = 0.0;
        self.entry_price = None;
        self.side = PositionSide::Flat;
        self.take_profit = None;
        self.stop_loss = None;
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
}

#[derive(Clone, Default)]
struct SideBook {
    levels: Vec<(f64, f64)>,
}

#[derive(Clone, Default)]
struct OrderBook {
    bids: SideBook,
    asks: SideBook,
}

impl OrderBook {
    fn from_midprice(mid: f64) -> Self {
        let mut bids = SideBook::default();
        let mut asks = SideBook::default();

        for i in 0..20 {
            let level = i as f64;
            let spread = 0.5 * level.max(1.0);
            let bid_price = mid - spread;
            let ask_price = mid + spread;
            let size = 0.1 + level * 0.01;
            bids.levels.push((bid_price, size));
            asks.levels.push((ask_price, size));
        }

        Self { bids, asks }
    }

    fn depth_points(&self) -> (Vec<(f64, f64)>, Vec<(f64, f64)>) {
        let mut bids = self.bids.levels.clone();
        bids.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());

        let mut asks = self.asks.levels.clone();
        asks.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());

        let mut bid_points = Vec::new();
        let mut ask_points = Vec::new();

        let mut cum = 0.0;
        for (p, s) in bids {
            cum += s;
            bid_points.push((p, cum));
        }

        cum = 0.0;
        for (p, s) in asks {
            cum += s;
            ask_points.push((p, cum));
        }

        (bid_points, ask_points)
    }
}

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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Orderbook,
    Candles,
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
    order_book: OrderBook,

    rng: StdRng,
}

impl MyApp {
    fn new() -> Self {
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
                x_zoom: 1.0,
                x_pan_secs: 0.0,
            },

            trading: TradingState::new(),
            order_book: OrderBook::from_midprice(3000.0),

            rng: StdRng::seed_from_u64(42),
        }
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
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let step: f64 = self.rng.random_range(-2.0..2.0);
        self.last_price = (self.last_price + step).clamp(2950.0, 3050.0);

        self.trading.check_tp_sl(self.last_price);
        self.order_book = OrderBook::from_midprice(self.last_price);

        self.tf_30s.update(ts, self.last_price);
        self.tf_1m.update(ts, self.last_price);
        self.tf_3m.update(ts, self.last_price);
        self.tf_5m.update(ts, self.last_price);
    }

    fn ui_top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.selected_tab, Tab::Orderbook, "Orderbook + Depth");
            ui.selectable_value(&mut self.selected_tab, Tab::Candles, "Candles + RSI");
            ui.separator();

            ui.label("TF:");
            if ui.button("30s").clicked() { self.selected_tf = 30; }
            if ui.button("1m").clicked() { self.selected_tf = 60; }
            if ui.button("3m").clicked() { self.selected_tf = 180; }
            if ui.button("5m").clicked() { self.selected_tf = 300; }

            ui.separator();
            ui.checkbox(&mut self.chart.auto_scale, "Auto-scale Y");
            ui.separator();
            ui.label(format!("Mark: {:.2}", self.last_price));
        });
    }

    fn ui_trading_panel(&mut self, ui: &mut egui::Ui) {
        ui.group(|ui| {
            ui.heading("Balances + Trade (fake)");

            ui.label(format!("Wallet USDC: {:.2}", self.trading.wallet_usdc));
            ui.label(format!("Margin USDC: {:.2}", self.trading.margin));
            ui.separator();

            // --- Deposit / Withdraw row ---
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
                    if ui.selectable_label(self.trading.side == side, side.label()).clicked() {
                        self.trading.side = side;
                    }
                }
            });

            ui.add(egui::Slider::new(&mut self.trading.leverage, 1.0..=50.0).text("Leverage (x)"));
            ui.add(egui::Slider::new(&mut self.trading.position, 0.0..=100.0).text("Position (units)"));

            ui.separator();

            ui.horizontal(|ui| {
                if ui.button("Open / Close").clicked() {
                    if self.trading.is_open() { self.trading.close_at(self.last_price); }
                    else { self.trading.open_at(self.last_price); }
                }
                if ui.button("TP +1").clicked() { self.trading.bump_tp(self.last_price, 1.0); }
                if ui.button("TP -1").clicked() { self.trading.bump_tp(self.last_price, -1.0); }
                if ui.button("SL +1").clicked() { self.trading.bump_sl(self.last_price, 1.0); }
                if ui.button("SL -1").clicked() { self.trading.bump_sl(self.last_price, -1.0); }
            });

            ui.separator();

            let upnl = self.trading.unrealized_pnl(self.last_price);
            let equity = self.trading.equity(self.last_price);

            ui.label(format!(
                "Side: {}, Pos: {:.4}, Lev: {:.2}x, Notional: {:.2}",
                self.trading.side.label(),
                self.trading.position,
                self.trading.leverage,
                self.trading.notional(),
            ));
            ui.label(format!(
                "Entry: {:.2}, uPnL: {:+.2}, rPnL: {:+.2}, Equity: {:.2}",
                self.trading.entry_price.unwrap_or(0.0),
                upnl,
                self.trading.realized_pnl,
                equity
            ));
            ui.label(format!(
                "TP: {}   SL: {}",
                self.trading.take_profit.map(|p| format!("{:.2}", p)).unwrap_or("-".into()),
                self.trading.stop_loss.map(|p| format!("{:.2}", p)).unwrap_or("-".into()),
            ));
        });
    }

    fn ui_orderbook(&mut self, ui: &mut egui::Ui) {
        let avail_h = ui.available_height();
        let avail_w = ui.available_width();

        ui.heading("Orderbook + Depth (fake)");

        ui.allocate_ui(egui::vec2(avail_w, avail_h), |ui| {
            ui.horizontal(|ui| {
                let left_w = avail_w * 0.45;
                let right_w = avail_w * 0.55;

                ui.allocate_ui(egui::vec2(left_w, avail_h), |ui| {
                    let (bids, asks) = self.order_book.depth_points();
                    Plot::new("depth_plot")
                        .height(avail_h * 0.9)
                        .show(ui, |plot_ui| {
                            if !bids.is_empty() {
                                let pts: PlotPoints = bids.iter().map(|(x,y)| [*x,*y]).collect::<Vec<_>>().into();
                                plot_ui.line(Line::new(pts).name("Bids"));
                            }
                            if !asks.is_empty() {
                                let pts: PlotPoints = asks.iter().map(|(x,y)| [*x,*y]).collect::<Vec<_>>().into();
                                plot_ui.line(Line::new(pts).name("Asks"));
                            }
                        });
                });

                ui.separator();

                ui.allocate_ui(egui::vec2(right_w, avail_h), |ui| {
                    ui.label("Top ladders");

                    ui.columns(2, |cols| {
                        cols[0].label("Bids");
                        egui::Grid::new("bids_grid").striped(true).show(&mut cols[0], |ui| {
                            ui.label("Price"); ui.label("Size"); ui.end_row();
                            let mut bids = self.order_book.bids.levels.clone();
                            bids.sort_by(|a,b| b.0.partial_cmp(&a.0).unwrap());
                            for (p,s) in bids.into_iter().take(15) {
                                ui.label(format!("{:>8.2}", p));
                                ui.label(format!("{:>6.4}", s));
                                ui.end_row();
                            }
                        });

                        cols[1].label("Asks");
                        egui::Grid::new("asks_grid").striped(true).show(&mut cols[1], |ui| {
                            ui.label("Price"); ui.label("Size"); ui.end_row();
                            let mut asks = self.order_book.asks.levels.clone();
                            asks.sort_by(|a,b| a.0.partial_cmp(&b.0).unwrap());
                            for (p,s) in asks.into_iter().take(15) {
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
            ui.label("No candles yet.");
            return;
        }

        ui.horizontal(|ui| {
            ui.label("History (candles):");
            ui.add(egui::Slider::new(&mut self.chart.show_candles, 20..=600).logarithmic(true));

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
        let candles_h = avail_h * 0.55;
        let rsi_h = avail_h * 0.25;
        let bottom_h = avail_h * 0.20;

        ui.allocate_ui(egui::vec2(avail_w, candles_h), |ui| {
            Plot::new("candles_plot")
                .height(candles_h)
                .include_y(y_min)
                .include_y(y_max)
                .show(ui, |plot_ui| {
                    let tf = self.selected_tf as f64;

                    let last = visible.last().unwrap();
                    let x_max = (last.t as f64 + tf) + self.chart.x_pan_secs;
                    let base_span = tf * self.chart.show_candles as f64;
                    let span = base_span / self.chart.x_zoom.max(1e-6);
                    let x_min = x_max - span;

                    plot_ui.set_plot_bounds(
                        PlotBounds::from_min_max([x_min, y_min], [x_max, y_max]),
                    );

                    for c in visible {
                        let left = c.t as f64;
                        let right = left + tf;
                        let mid = left + tf * 0.5;

                        let top = c.open.max(c.close);
                        let bot = c.open.min(c.close);

                        let color = if c.close >= c.open { Color32::GREEN } else { Color32::RED };

                        let wick_pts: PlotPoints = vec![[mid, c.low], [mid, c.high]].into();
                        plot_ui.line(Line::new(wick_pts).color(color));

                        let left_edge: PlotPoints = vec![[left, bot], [left, top]].into();
                        let right_edge: PlotPoints = vec![[right, bot], [right, top]].into();
                        let top_edge: PlotPoints = vec![[left, top], [right, top]].into();
                        let bot_edge: PlotPoints = vec![[left, bot], [right, bot]].into();

                        plot_ui.line(Line::new(left_edge).color(color).width(2.0));
                        plot_ui.line(Line::new(right_edge).color(color).width(2.0));
                        plot_ui.line(Line::new(top_edge).color(color).width(2.0));
                        plot_ui.line(Line::new(bot_edge).color(color).width(2.0));
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
                });
        });

        ui.separator();

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
                .show(ui, |plot_ui| {
                    let tf = self.selected_tf as f64;
                    let last = visible.last().unwrap();
                    let x_max = (last.t as f64 + tf) + self.chart.x_pan_secs;
                    let base_span = tf * self.chart.show_candles as f64;
                    let span = base_span / self.chart.x_zoom.max(1e-6);
                    let x_min = x_max - span;

                    plot_ui.set_plot_bounds(
                        PlotBounds::from_min_max([x_min, 0.0], [x_max, 100.0]),
                    );

                    if !rsi_visible.is_empty() {
                        let pts: PlotPoints = rsi_visible.iter().map(|(t,v)| [*t,*v]).collect::<Vec<_>>().into();
                        plot_ui.line(Line::new(pts).name("RSI"));
                        plot_ui.hline(HLine::new(70.0));
                        plot_ui.hline(HLine::new(30.0));
                    }
                });
        });

        ui.separator();

        ui.allocate_ui(egui::vec2(avail_w, bottom_h), |ui| {
            ui.columns(2, |cols| {
                cols[0].group(|ui| {
                    ui.label("Last candle:");
                    if let Some(c) = series_vec.last() {
                        ui.label(format!("t_start unix: {}", c.t));
                        ui.label(format!("O: {:.2}", c.open));
                        ui.label(format!("H: {:.2}", c.high));
                        ui.label(format!("L: {:.2}", c.low));
                        ui.label(format!("C: {:.2}", c.close));
                    }
                });

                self.ui_trading_panel(&mut cols[1]);
            });
        });
    }
}

impl eframe::App for MyApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.tick();

        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            self.ui_top_bar(ui);
        });

        egui::CentralPanel::default().show(ctx, |ui| match self.selected_tab {
            Tab::Orderbook => self.ui_orderbook(ui),
            Tab::Candles => self.ui_candles(ui),
        });

        ctx.request_repaint_after(Duration::from_millis(33));
    }
}

fn main() {
    let options = eframe::NativeOptions::default();
    if let Err(e) = eframe::run_native(
        "Ladder GUI (egui)",
        options,
        Box::new(|_cc| Box::new(MyApp::new())),
    ) {
        eprintln!("eframe error: {e}");
    }
}
