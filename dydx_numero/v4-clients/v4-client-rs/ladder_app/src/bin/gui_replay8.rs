// ladder_app/src/bin/gui_replay8.rs
//
// Offline replay GUI for dYdX testnet CSV logs produced by gui_app28.
// - Reads data/orderbook_{TICKER}.csv and data/trades_{TICKER}.csv
// - Reconstructs orderbook snapshot + candles + volume + trades up to a replay timestamp
// - Supports ETH-USD, BTC-USD, SOL-USD (if files exist)
//
// Run (no mnemonic needed for replay):
//   cargo run -p ladder_app --bin gui_replay8
//

mod candle_agg;

use candle_agg::{Candle, CandleAgg};

use eframe::egui;
use egui::Color32;
use egui_plot::{HLine, Line, Plot, PlotBounds, PlotPoints, VLine};

use std::cmp::{max, min};
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use chrono::{Local, TimeZone};

// ---------------- price key helpers (for BTreeMap) ----------------

type PriceKey = i64;

fn price_to_key(price: f64) -> PriceKey {
    // 1e-4 precision
    (price * 10_000.0).round() as PriceKey
}

fn key_to_price(key: PriceKey) -> f64 {
    key as f64 / 10_000.0
}

// ---------------- CSV data structures ----------------

#[derive(Clone, Debug)]
struct BookCsvEvent {
    ts: u64,
    ticker: String,
    kind: String, // "book" or "book_delta"
    side: String, // "bid" or "ask"
    price: f64,
    size: f64,
}

#[derive(Clone, Debug)]
struct TradeCsvEvent {
    ts: u64,
    ticker: String,
    source: String, // "real"
    side: String,   // "buy" / "sell"
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

// Snapshot used for drawing at a replay timestamp
#[derive(Clone, Debug, Default)]
struct Snapshot {
    // book in integer price space
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

// ---------------- Time display mode ----------------

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

// ---------------- Chart settings ----------------

#[derive(Clone)]
struct ChartSettings {
    show_candles: usize,
    auto_scale: bool,
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
            auto_scale: true,
            y_min: 0.0,
            y_max: 0.0,
            x_zoom: 1.0,
            x_pan_secs: 0.0,
            selected_tf: 60,
        }
    }
}

// ---------------- Loading CSVs ----------------

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

// Compute snapshot for given ticker & ts by replaying from scratch
fn compute_snapshot_for(data: &TickerData, target_ts: u64) -> Snapshot {
    let mut bids: BTreeMap<PriceKey, f64> = BTreeMap::new();
    let mut asks: BTreeMap<PriceKey, f64> = BTreeMap::new();

    let mut tf_30s = CandleAgg::new(30);
    let mut tf_1m = CandleAgg::new(60);
    let mut tf_3m = CandleAgg::new(180);
    let mut tf_5m = CandleAgg::new(300);

    // replay orderbook events
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

        // after applying this level update, if we have a mid, update candles
        if let (Some((bp, _)), Some((ap, _))) = (bids.iter().next_back(), asks.iter().next()) {
            let mid = (key_to_price(*bp) + key_to_price(*ap)) * 0.5;
            let vol = e.size.abs();

            tf_30s.update(e.ts, mid, vol);
            tf_1m.update(e.ts, mid, vol);
            tf_3m.update(e.ts, mid, vol);
            tf_5m.update(e.ts, mid, vol);
        }
    }

    // trades up to target_ts (take last 200)
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

    // last mid from the 1m candles for display
    let series_1m = tf_1m.get_series();
    let (last_mid, last_vol) = if let Some(c) = series_1m.last() {
        (c.close, c.volume)
    } else {
        (0.0, 0.0)
    };

    Snapshot {
        bids,
        asks,
        tf_30s: tf_30s.get_series(),
        tf_1m: tf_1m.get_series(),
        tf_3m: tf_3m.get_series(),
        tf_5m: tf_5m.get_series(),
        last_mid,
        last_vol,
        trades,
    }
}

// ---------------- Replay app ----------------

struct ReplayApp {
    tickers: Vec<String>,
    data: HashMap<String, TickerData>,
    current_ticker: String,

    current_ts: u64,
    time_mode: TimeDisplayMode,
    chart: ChartSettings,
}

impl ReplayApp {
    fn new(base_dir: &str) -> Self {
        let mut data = HashMap::new();
        let mut tickers = Vec::new();

        for tk in &["ETH-USD", "BTC-USD", "SOL-USD"] {
            if let Some(td) = load_ticker_data(base_dir, tk) {
                tickers.push(tk.to_string());
                data.insert(tk.to_string(), td);
            }
        }

        if tickers.is_empty() {
            tickers.push("ETH-USD".to_string());
        }

        let current_ticker = tickers[0].clone();

        let current_ts = data
            .get(&current_ticker)
            .map(|td| td.max_ts)
            .unwrap_or(0);

        Self {
            tickers,
            data,
            current_ticker,
            current_ts,
            time_mode: TimeDisplayMode::Local,
            chart: ChartSettings::default(),
        }
    }

    fn current_ticker_data(&self) -> Option<&TickerData> {
        self.data.get(&self.current_ticker)
    }

    fn ensure_ts_in_range(&mut self) {
        let (min_ts, max_ts) = if let Some(td) = self.current_ticker_data() {
            (td.min_ts, td.max_ts)
        } else {
            return;
        };

        if self.current_ts < min_ts {
            self.current_ts = min_ts;
        }
        if self.current_ts > max_ts {
            self.current_ts = max_ts;
        }
    }

    fn ui_top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            // ticker menu
            let tickers = self.tickers.clone();
            ui.menu_button(format!("Ticker: {}", self.current_ticker), |ui| {
                for t in &tickers {
                    let selected = *t == self.current_ticker;
                    if ui.selectable_label(selected, t).clicked() {
                        self.current_ticker = t.clone();
                        if let Some(td) = self.data.get(t) {
                            self.current_ts = td.max_ts;
                        }
                        ui.close_menu();
                    }
                }
            });

            ui.separator();

            ui.label("Time:");
            ui.horizontal(|ui| {
                for mode in [TimeDisplayMode::Local, TimeDisplayMode::Unix] {
                    if ui
                        .selectable_label(self.time_mode == mode, mode.label())
                        .clicked()
                    {
                        self.time_mode = mode;
                    }
                }
            });

            if let Some(td) = self.current_ticker_data() {
                ui.separator();
                ui.label(format!(
                    "Range: {} → {}",
                    format_ts(self.time_mode, td.min_ts),
                    format_ts(self.time_mode, td.max_ts)
                ));
                ui.separator();
                ui.label(format!(
                    "Current: {}",
                    format_ts(self.time_mode, self.current_ts)
                ));
            } else {
                ui.separator();
                ui.label("No data loaded for this ticker.");
            }
        });

        ui.separator();

        if let Some(td) = self.current_ticker_data() {
            let mut ts = self.current_ts;
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
            self.current_ts = ts;
        } else {
            ui.label("No CSV data available.");
        }

        ui.separator();

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
            for (label, tf) in [("30s", 30u64), ("1m", 60), ("3m", 180), ("5m", 300)] {
                if ui
                    .selectable_label(self.chart.selected_tf == tf, label)
                    .clicked()
                {
                    self.chart.selected_tf = tf;
                }
            }

            ui.separator();
            ui.checkbox(&mut self.chart.auto_scale, "Auto Y");
        });
    }

    fn ui_orderbook(&self, ui: &mut egui::Ui, snap: &Snapshot) {
        ui.heading(format!(
            "Orderbook snapshot for {} @ {}",
            self.current_ticker,
            format_ts(self.time_mode, self.current_ts)
        ));

        let avail_w = ui.available_width();
        let avail_h = ui.available_height();
        let depth_w = avail_w * 0.45;
        let ladders_w = avail_w * 0.55;

        ui.horizontal(|ui| {
            // Depth plot
            ui.allocate_ui(egui::vec2(depth_w, avail_h), |ui| {
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

                Plot::new("depth_replay")
                    .height(avail_h * 0.9)
                    .show(ui, |plot_ui| {
                        if !bid_points.is_empty() {
                            let pts: PlotPoints = bid_points
                                .iter()
                                .map(|(x, y)| [*x, *y])
                                .collect::<Vec<_>>()
                                .into();
                            plot_ui.line(Line::new(pts).name("Bids"));
                        }
                        if !ask_points.is_empty() {
                            let pts: PlotPoints = ask_points
                                .iter()
                                .map(|(x, y)| [*x, *y])
                                .collect::<Vec<_>>()
                                .into();
                            plot_ui.line(Line::new(pts).name("Asks"));
                        }
                    });
            });

            ui.separator();

            // Ladders + trade info
            ui.allocate_ui(egui::vec2(ladders_w, avail_h), |ui| {
                ui.label("Top ladders");

                ui.columns(2, |cols| {
                    cols[0].label("Bids");
                    egui::Grid::new("bids_grid_replay")
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

                    cols[1].label("Asks");
                    egui::Grid::new("asks_grid_replay")
                        .striped(true)
                        .show(&mut cols[1], |ui| {
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

                ui.separator();
                ui.label(format!(
                    "Last mid: {:.2}   Last vol: {:.4}",
                    snap.last_mid, snap.last_vol
                ));

                ui.separator();
                ui.label("Recent trades (up to current time):");
                egui::ScrollArea::vertical()
                    .max_height(avail_h * 0.4)
                    .show(ui, |ui| {
                        egui::Grid::new("trades_grid_replay")
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
            });
        });
    }

    fn ui_candles(&self, ui: &mut egui::Ui, snap: &Snapshot) {
        let series_vec = match self.chart.selected_tf {
            30 => &snap.tf_30s,
            60 => &snap.tf_1m,
            180 => &snap.tf_3m,
            300 => &snap.tf_5m,
            _ => &snap.tf_1m,
        };

        if series_vec.is_empty() {
            ui.label("No candles yet for this ticker/time.");
            return;
        }

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

        let candles_h = avail_h * 0.7;
        let volume_h = avail_h * 0.3;

        // ---- Candles plot ----
        ui.allocate_ui(egui::vec2(avail_w, candles_h), |ui| {
            Plot::new("candles_replay")
                .height(candles_h)
                .include_y(y_min)
                .include_y(y_max)
                .allow_drag(true)
                .allow_zoom(true)
                .show(ui, |plot_ui| {
                    let tf = self.chart.selected_tf as f64;
                    let last = visible.last().unwrap();
                    let x_center = last.t as f64 + tf * 0.5;
                    let base_span = tf * self.chart.show_candles as f64;
                    let span = base_span / self.chart.x_zoom.max(1e-6);
                    let x_min = x_center - span * 0.5 + self.chart.x_pan_secs;
                    let x_max = x_center + span * 0.5 + self.chart.x_pan_secs;

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

                        // filled rectangle body
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

                    // now price/time lines
                    let now_x = self.current_ts as f64;
                    if let Some(last) = series_vec.last() {
                        plot_ui.hline(HLine::new(last.close).name("last_close"));
                    }
                    plot_ui.vline(VLine::new(now_x).name("now_ts"));
                });
        });

        ui.separator();

        // ---- Volume plot ----
        ui.allocate_ui(egui::vec2(avail_w, volume_h), |ui| {
            Plot::new("volume_replay")
                .height(volume_h)
                .include_y(0.0)
                .show(ui, |plot_ui| {
                    let tf = self.chart.selected_tf as f64;
                    let last = visible.last().unwrap();
                    let x_center = last.t as f64 + tf * 0.5;
                    let base_span = tf * self.chart.show_candles as f64;
                    let span = base_span / self.chart.x_zoom.max(1e-6);
                    let x_min = x_center - span * 0.5 + self.chart.x_pan_secs;
                    let x_max = x_center + span * 0.5 + self.chart.x_pan_secs;

                    let max_vol = visible
                        .iter()
                        .map(|c| c.volume)
                        .fold(0.0_f64, f64::max)
                        .max(1e-6);
                    let y_max = max_vol * 1.1;

                    plot_ui.set_plot_bounds(PlotBounds::from_min_max(
                        [x_min, 0.0],
                        [x_max, y_max],
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
        });
    }
}

impl eframe::App for ReplayApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.ensure_ts_in_range();

        let snapshot = self
            .current_ticker_data()
            .map(|td| compute_snapshot_for(td, self.current_ts));

        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            self.ui_top_bar(ui);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(snap) = &snapshot {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.heading("Replay viewer");
                        ui.label(format!(
                            "Ticker: {}   Current ts: {}",
                            self.current_ticker,
                            format_ts(self.time_mode, self.current_ts)
                        ));
                        ui.separator();

                        self.ui_orderbook(ui, snap);
                        ui.separator();
                        ui.separator();

                        self.ui_candles(ui, snap);
                    });
            } else {
                ui.heading("No data loaded.");
                ui.label("Make sure CSV files exist under ./data:");
                ui.label("  data/orderbook_ETH-USD.csv");
                ui.label("  data/trades_ETH-USD.csv");
                ui.label("  ... and similarly for BTC-USD, SOL-USD");
            }
        });

        ctx.request_repaint_after(std::time::Duration::from_millis(50));
    }
}

fn main() {
    let base_dir = "data"; // same dir gui_app28 writes into

    let options = eframe::NativeOptions::default();
    let app = ReplayApp::new(base_dir);

    if let Err(e) = eframe::run_native(
        "dYdX Replay (offline)",
        options,
        Box::new(|_cc| Box::new(app)),
    ) {
        eprintln!("eframe error: {e}");
    }
}
