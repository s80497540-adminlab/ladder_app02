mod candle_agg;

slint::include_modules!();

use crate::candle_agg::{Candle, CandleAgg};

use std::cell::RefCell;
use std::cmp::{max, min};
use std::collections::{BTreeMap, HashMap};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{Local, TimeZone};
use rhai::{Engine, Scope};

use slint::{ModelRc, SharedString, Timer, TimerMode, VecModel};

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_secs()
}

// ---- price key helpers -----------------------------------------------------

type PriceKey = i64;

fn price_to_key(price: f64) -> PriceKey {
    (price * 10_000.0).round() as PriceKey
}

fn key_to_price(key: PriceKey) -> f64 {
    key as f64 / 10_000.0
}

fn format_ts_local(ts: u64) -> String {
    let dt = Local
        .timestamp_opt(ts as i64, 0)
        .single()
        .unwrap_or_else(|| Local.timestamp_opt(0, 0).single().unwrap());
    dt.format("%Y-%m-%d %H:%M:%S").to_string()
}

// ---- CSV data structures ---------------------------------------------------

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

#[derive(Clone, Debug, Default)]
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
    candles: Vec<Candle>,
    trades: Vec<TradeCsvEvent>,
    last_mid: f64,
    last_vol: f64,
}

#[derive(Clone, Debug, Default)]
struct BubbleMetrics {
    best_bid: f64,
    best_ask: f64,
    mid: f64,
    spread: f64,
    bid_liq: f64,
    ask_liq: f64,
    imbalance: f64,
}

// ---- CSV loading -----------------------------------------------------------

fn load_book_csv(path: &Path, ticker: &str) -> Vec<BookCsvEvent> {
    if !path.exists() {
        return Vec::new();
    }
    let Ok(f) = File::open(path) else {
        return Vec::new();
    };
    let reader = BufReader::new(f);
    let mut out = Vec::new();

    for line in reader.lines().flatten() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split(',').collect();
        if parts.len() < 6 {
            continue;
        }

        let Ok(ts) = parts[0].parse::<u64>() else {
            continue;
        };
        let tk = parts[1].trim_matches('"').to_string();
        if tk != ticker {
            continue;
        }
        let kind = parts[2].to_string();
        let side = parts[3].to_string();
        let Ok(price) = parts[4].parse::<f64>() else {
            continue;
        };
        let Ok(size) = parts[5].parse::<f64>() else {
            continue;
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

    out.sort_by_key(|e| e.ts);
    out
}

fn load_trades_csv(path: &Path, ticker: &str) -> Vec<TradeCsvEvent> {
    if !path.exists() {
        return Vec::new();
    }
    let Ok(f) = File::open(path) else {
        return Vec::new();
    };
    let reader = BufReader::new(f);
    let mut out = Vec::new();

    for line in reader.lines().flatten() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split(',').collect();
        if parts.len() < 5 {
            continue;
        }

        let Ok(ts) = parts[0].parse::<u64>() else {
            continue;
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

    out.sort_by_key(|t| t.ts);
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

// ---- snapshot + metrics ----------------------------------------------------

fn compute_snapshot_for(data: &TickerData, tf_secs: u64, window_secs: u64) -> Snapshot {
    let mut bids: BTreeMap<PriceKey, f64> = BTreeMap::new();
    let mut asks: BTreeMap<PriceKey, f64> = BTreeMap::new();

    if data.book_events.is_empty() {
        return Snapshot::default();
    }

    let target_ts = data.max_ts;
    let window_start = target_ts.saturating_sub(window_secs);

    let mut agg = CandleAgg::new(tf_secs);
    let _tf_for_debug = agg.tf();

    for e in &data.book_events {
        if !e.ticker.is_empty() && e.ticker != data.ticker {
            // inconsistent line, ignore silently
        }
        if !e.kind.is_empty() && e.kind != "orderbook" {
            // other kinds could be special; just acknowledged
        }

        if e.ts < window_start {
            continue;
        }
        if e.ts > target_ts {
            break;
        }

        let map = if e.side.eq_ignore_ascii_case("bid") {
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
            let vol = e.size.abs();
            agg.update(e.ts, mid, vol);
        }
    }

    {
        let s = agg.series_mut();
        let max_candles = 500usize;
        if s.len() > max_candles {
            let extra = s.len() - max_candles;
            s.drain(0..extra);
        }
    }

    let candles = agg.series().clone();
    let (last_mid, last_vol) = if let Some(c) = candles.last() {
        (c.close, c.volume)
    } else {
        (0.0, 0.0)
    };

    let mut trades: Vec<TradeCsvEvent> = data
        .trade_events
        .iter()
        .filter(|t| t.ts >= window_start && t.ts <= target_ts)
        .cloned()
        .collect();

    trades.sort_by_key(|t| t.ts);
    if trades.len() > 100 {
        let start = trades.len() - 100;
        trades = trades[start..].to_vec();
    }

    Snapshot {
        bids,
        asks,
        candles,
        trades,
        last_mid,
        last_vol,
    }
}

fn compute_bubble_metrics(snap: &Snapshot) -> BubbleMetrics {
    let best_bid = snap
        .bids
        .iter()
        .next_back()
        .map(|(k, _)| key_to_price(*k))
        .unwrap_or(0.0);
    let best_ask = snap
        .asks
        .iter()
        .next()
        .map(|(k, _)| key_to_price(*k))
        .unwrap_or(0.0);

    let mid = if best_bid > 0.0 && best_ask > 0.0 {
        (best_bid + best_ask) * 0.5
    } else {
        0.0
    };

    let spread = if best_bid > 0.0 && best_ask > 0.0 {
        best_ask - best_bid
    } else {
        0.0
    };

    let mut bid_liq = 0.0;
    for (_, s) in snap.bids.iter().rev().take(10) {
        bid_liq += *s;
    }
    let mut ask_liq = 0.0;
    for (_, s) in snap.asks.iter().take(10) {
        ask_liq += *s;
    }

    let imbalance = if ask_liq > 0.0 { bid_liq / ask_liq } else { 0.0 };

    BubbleMetrics {
        best_bid,
        best_ask,
        mid,
        spread,
        bid_liq,
        ask_liq,
        imbalance,
    }
}

// ---- CSV append for trades (GUI & bot) ------------------------------------

fn append_trade_csv(base_dir: &Path, ticker: &str, source: &str, side: &str, size_str: &str) {
    let ts = now_unix();
    let path = base_dir.join(format!("trades_{ticker}.csv"));

    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{ts},{ticker},{source},{side},{size_str}");
    }
}

// ---- App core --------------------------------------------------------------

struct AppCore {
    base_dir: PathBuf,
    tickers: Vec<String>,
    ticker_data: HashMap<String, TickerData>,
    current_ticker: String,
    tf_secs: u64,
    window_secs: u64,
    last_reload_ts: u64,

    engine: Engine,
    scope: Scope<'static>,
    script_error: String,
    bot_signal: String,
    bot_size: f64,
    bot_comment: String,
    bot_auto_trade: bool,
    last_bot_fired_signal: String,

    receipts: Vec<Receipt>,

    // Cached snapshot + metrics to avoid recomputing every second.
    cached_snapshot: Option<Snapshot>,
    cached_metrics: Option<BubbleMetrics>,
    snapshot_dirty: bool,

    // DOM zoom depth (how many levels to show)
    dom_depth_levels: usize,
}

impl AppCore {
    fn new(base_dir: PathBuf, tickers: Vec<String>) -> Self {
        let mut ticker_data = HashMap::new();

        println!("AppCore::new: loading CSV data from {}", base_dir.display());

        for tk in &tickers {
            if let Some(td) = load_ticker_data(&base_dir, tk) {
                println!(
                    "  {}: events={}, trades={}, ts {}..{}",
                    td.ticker,
                    td.book_events.len(),
                    td.trade_events.len(),
                    td.min_ts,
                    td.max_ts
                );

                let mut agg = CandleAgg::new(60);
                let candles_path = base_dir.join(format!("candles_{}.csv", tk));
                agg.load_from_csv(&candles_path);
                agg.save_to_csv(&candles_path);

                ticker_data.insert(tk.clone(), td);
            } else {
                println!("  {}: no CSV data found", tk);
            }
        }

        let current_ticker = tickers
            .get(0)
            .cloned()
            .unwrap_or_else(|| "ETH-USD".to_string());

        let mut engine = Engine::new();
        engine.set_max_expr_depths(64, 64);

        let scope = Scope::new();

        Self {
            base_dir,
            tickers,
            ticker_data,
            current_ticker,
            tf_secs: 60,
            window_secs: 3600,
            last_reload_ts: now_unix(),
            engine,
            scope,
            script_error: String::new(),
            bot_signal: "none".to_string(),
            bot_size: 0.0,
            bot_comment: String::new(),
            bot_auto_trade: false,
            last_bot_fired_signal: "none".to_string(),
            receipts: Vec::new(),
            cached_snapshot: None,
            cached_metrics: None,
            snapshot_dirty: true,
            dom_depth_levels: 20,
        }
    }

    fn mark_snapshot_dirty(&mut self) {
        self.snapshot_dirty = true;
    }

    fn recompute_snapshot_if_dirty(&mut self) {
        if !self.snapshot_dirty {
            return;
        }

        if let Some(td) = self.ticker_data.get(&self.current_ticker) {
            let snap = compute_snapshot_for(td, self.tf_secs, self.window_secs);
            let metrics = compute_bubble_metrics(&snap);
            self.cached_snapshot = Some(snap);
            self.cached_metrics = Some(metrics);
        } else {
            self.cached_snapshot = None;
            self.cached_metrics = None;
        }

        self.snapshot_dirty = false;
    }

    fn snapshot_for_ui(&mut self) -> Option<(Snapshot, BubbleMetrics)> {
        self.recompute_snapshot_if_dirty();
        match (&self.cached_snapshot, &self.cached_metrics) {
            (Some(s), Some(m)) => Some((s.clone(), m.clone())),
            _ => None,
        }
    }

    fn ticker_range(&self, ticker: &str) -> Option<(u64, u64)> {
        self.ticker_data.get(ticker).map(|td| (td.min_ts, td.max_ts))
    }

    fn reload_current_ticker(&mut self) {
        if let Some(td) = load_ticker_data(&self.base_dir, &self.current_ticker) {
            println!(
                "[RELOAD] {}: events={}, trades={}, ts {}..{}",
                td.ticker,
                td.book_events.len(),
                td.trade_events.len(),
                td.min_ts,
                td.max_ts
            );
            self.ticker_data.insert(self.current_ticker.clone(), td);
            self.last_reload_ts = now_unix();
            self.mark_snapshot_dirty();
        }
    }

    fn set_tf_from_ui(&mut self, new_tf_secs: u64) {
        if new_tf_secs == 0 || new_tf_secs == self.tf_secs {
            return;
        }
        println!(
            "[TF] changing candle tf from {} to {} seconds",
            self.tf_secs, new_tf_secs
        );
        self.tf_secs = new_tf_secs;
        self.mark_snapshot_dirty();
    }

    fn set_window_from_ui(&mut self, minutes: u64) {
        if minutes == 0 {
            return;
        }
        let new_secs = minutes.saturating_mul(60);
        if new_secs == self.window_secs {
            return;
        }
        println!(
            "[WINDOW] changing window from {}s to {}s",
            self.window_secs, new_secs
        );
        self.window_secs = new_secs;
        self.mark_snapshot_dirty();
    }

    fn set_dom_depth_from_ui(&mut self, levels: i32) {
        let mut lv = if levels < 1 { 1 } else { levels } as usize;
        if lv < 5 {
            lv = 5;
        }
        if lv > 50 {
            lv = 50;
        }
        if lv == self.dom_depth_levels {
            return;
        }
        println!(
            "[DOM] changing depth levels from {} to {}",
            self.dom_depth_levels, lv
        );
        self.dom_depth_levels = lv;
    }

    fn dom_depth_levels(&self) -> usize {
        self.dom_depth_levels
    }

    fn run_bot_script(&mut self, app: &AppWindow, metrics: &BubbleMetrics) {
        if !self.script_error.is_empty() {
            eprintln!("[SCRIPT] previous error: {}", self.script_error);
        }

        self.script_error.clear();

        let script: String = app.get_script_text().to_string();

        self.scope.clear();

        self.scope.set_value("ticker", self.current_ticker.clone());
        self.scope.set_value("best_bid", metrics.best_bid);
        self.scope.set_value("best_ask", metrics.best_ask);
        self.scope.set_value("mid", metrics.mid);
        self.scope.set_value("spread", metrics.spread);
        self.scope.set_value("bid_liquidity_near", metrics.bid_liq);
        self.scope.set_value("ask_liquidity_near", metrics.ask_liq);
        self.scope.set_value("tf_secs", self.tf_secs as i64);

        self.scope.set_value("bot_signal", self.bot_signal.clone());
        self.scope.set_value("bot_size", self.bot_size);
        self.scope.set_value("bot_comment", self.bot_comment.clone());

        let res = self
            .engine
            .eval_with_scope::<()>(&mut self.scope, &script);

        match res {
            Ok(()) => {
                if let Some(sig) = self.scope.get_value::<String>("bot_signal") {
                    self.bot_signal = sig;
                } else {
                    self.bot_signal = "none".to_string();
                }
                if let Some(sz) = self.scope.get_value::<f64>("bot_size") {
                    self.bot_size = sz.max(0.0);
                } else {
                    self.bot_size = 0.0;
                }
                if let Some(cmt) = self.scope.get_value::<String>("bot_comment") {
                    self.bot_comment = cmt;
                } else {
                    self.bot_comment.clear();
                }

                self.script_error.clear();
                app.set_script_error(SharedString::from(""));
            }
            Err(e) => {
                self.bot_signal = "none".to_string();
                self.bot_size = 0.0;
                self.bot_comment.clear();
                self.script_error = e.to_string();
                app.set_script_error(SharedString::from(&self.script_error));
                eprintln!("[SCRIPT] error: {}", self.script_error);
            }
        }

        app.set_bot_signal(SharedString::from(&self.bot_signal));
        app.set_bot_size(self.bot_size as f32);
        app.set_bot_comment(SharedString::from(&self.bot_comment));
    }

    fn push_receipt(&mut self, app: &AppWindow, r: Receipt) {
        self.receipts.push(r);
        if self.receipts.len() > 300 {
            let extra = self.receipts.len() - 300;
            self.receipts.drain(0..extra);
        }
        let model = VecModel::from(self.receipts.clone());
        app.set_receipts(ModelRc::new(model));
    }

    fn maybe_auto_trade(&mut self, app: &AppWindow, metrics: &BubbleMetrics) {
        self.bot_auto_trade = app.get_bot_auto_trade();

        if !self.bot_auto_trade {
            return;
        }
        if self.bot_signal != "buy" && self.bot_signal != "sell" {
            return;
        }
        if self.bot_signal == self.last_bot_fired_signal {
            return;
        }
        if self.bot_size <= 0.0 {
            return;
        }

        let side = self.bot_signal.clone();
        let ticker = self.current_ticker.clone();
        let size_str = format!("{:.8}", self.bot_size);

        append_trade_csv(&self.base_dir, &ticker, "bot_auto", &side, &size_str);

        let receipt = Receipt {
            ts: SharedString::from(format_ts_local(now_unix())),
            ticker: SharedString::from(&ticker),
            side: SharedString::from(&side),
            kind: SharedString::from("BotAuto"),
            size: SharedString::from(&size_str),
            status: SharedString::from("submitted"),
            comment: SharedString::from(&self.bot_comment),
        };
        self.push_receipt(app, receipt);

        self.last_bot_fired_signal = self.bot_signal.clone();

        eprintln!(
            "[BOT] auto-trade: {} {} size {} (mid {:.2}, spread {:.5})",
            side, ticker, size_str, metrics.mid, metrics.spread
        );
    }
}

// ---- UI wiring -------------------------------------------------------------

fn apply_snapshot_to_ui(app: &AppWindow, snap: &Snapshot, metrics: &BubbleMetrics, dom_depth_levels: usize) {
    app.set_mid_price(metrics.mid as f32);
    app.set_best_bid(metrics.best_bid as f32);
    app.set_best_ask(metrics.best_ask as f32);
    app.set_spread(metrics.spread as f32);
    app.set_imbalance(metrics.imbalance as f32);

    let depth = dom_depth_levels.max(1).min(50);

    let mut bid_levels_raw: Vec<(PriceKey, f64)> =
        snap.bids.iter().rev().take(depth).map(|(k, s)| (*k, *s)).collect();
    let mut ask_levels_raw: Vec<(PriceKey, f64)> =
        snap.asks.iter().take(depth).map(|(k, s)| (*k, *s)).collect();

    let max_bid = bid_levels_raw
        .iter()
        .fold(0.0f64, |acc, (_, s)| acc.max(s.abs()));
    let max_ask = ask_levels_raw
        .iter()
        .fold(0.0f64, |acc, (_, s)| acc.max(s.abs()));

    let mut first_bid = true;
    let bids: Vec<BookLevel> = bid_levels_raw
        .drain(..)
        .map(|(k, s)| {
            let ratio = if max_bid > 0.0 { (s.abs() / max_bid) as f32 } else { 0.0 };
            let is_best = first_bid;
            if first_bid {
                first_bid = false;
            }
            BookLevel {
                price: SharedString::from(format!("{:.2}", key_to_price(k))),
                size: SharedString::from(format!("{:.4}", s)),
                depth_ratio: ratio,
                is_best,
            }
        })
        .collect();
    app.set_bids(ModelRc::new(VecModel::from(bids)));

    let mut first_ask = true;
    let asks: Vec<BookLevel> = ask_levels_raw
        .drain(..)
        .map(|(k, s)| {
            let ratio = if max_ask > 0.0 { (s.abs() / max_ask) as f32 } else { 0.0 };
            let is_best = first_ask;
            if first_ask {
                first_ask = false;
            }
            BookLevel {
                price: SharedString::from(format!("{:.2}", key_to_price(k))),
                size: SharedString::from(format!("{:.4}", s)),
                depth_ratio: ratio,
                is_best,
            }
        })
        .collect();
    app.set_asks(ModelRc::new(VecModel::from(asks)));

    let trades: Vec<Trade> = snap
        .trades
        .iter()
        .rev()
        .take(50)
        .map(|t| {
            let is_buy = t.side.to_ascii_lowercase().starts_with('b');
            let side_label = if t.source.is_empty() {
                t.side.clone()
            } else {
                format!("{} ({})", t.side, t.source)
            };
            let ts_str = format_ts_local(t.ts);

            Trade {
                ts: SharedString::from(ts_str),
                side: SharedString::from(side_label),
                size: SharedString::from(&t.size_str),
                is_buy,
            }
        })
        .collect();
    app.set_recent_trades(ModelRc::new(VecModel::from(trades)));

    let candle_rows: Vec<CandleRow> = snap
        .candles
        .iter()
        .rev()
        .take(200)
        .map(|c| CandleRow {
            ts: SharedString::from(format_ts_local(c.t)),
            open: SharedString::from(format!("{:.2}", c.open)),
            high: SharedString::from(format!("{:.2}", c.high)),
            low: SharedString::from(format!("{:.2}", c.low)),
            close: SharedString::from(format!("{:.2}", c.close)),
            volume: SharedString::from(format!("{:.4}", c.volume)),
        })
        .collect();
    app.set_candles(ModelRc::new(VecModel::from(candle_rows)));

    let mut candle_points_vec: Vec<CandlePoint> = Vec::new();
    let mut midline_n: f32 = 0.5;
    let mut last_move_str = "flat".to_string();

    if !snap.candles.is_empty() {
        let slice = &snap.candles[..];

        let mut min_price = f64::MAX;
        let mut max_price = f64::MIN;
        let mut max_vol = 0.0;

        for c in slice {
            if c.low < min_price {
                min_price = c.low;
            }
            if c.high > max_price {
                max_price = c.high;
            }
            if c.volume > max_vol {
                max_vol = c.volume;
            }
        }
        if !min_price.is_finite() || !max_price.is_finite() || max_price <= min_price {
            min_price = 0.0;
            max_price = 1.0;
        }
        let range = max_price - min_price;

        let norm_price = |p: f64| -> f32 {
            if range <= 0.0 {
                0.5
            } else {
                ((max_price - p) / range) as f32
            }
        };

        let n = slice.len();
        for (i, c) in slice.iter().enumerate() {
            let x_center = if n <= 1 { 0.5f32 } else { (i as f32 + 0.5) / n as f32 };
            let w = if n == 0 { 1.0f32 } else { (1.0f32 / n as f32) * 0.7 };

            let open_n = norm_price(c.open);
            let high_n = norm_price(c.high);
            let low_n = norm_price(c.low);
            let close_n = norm_price(c.close);
            let is_up = c.close >= c.open;

            let volume_n = if max_vol > 0.0 { (c.volume / max_vol) as f32 } else { 0.0 };

            candle_points_vec.push(CandlePoint {
                x: x_center,
                w,
                open: open_n,
                high: high_n,
                low: low_n,
                close: close_n,
                is_up,
                volume: volume_n,
            });
        }

        if range > 0.0 && metrics.mid.is_finite() {
            let clamped = metrics.mid.max(min_price).min(max_price);
            midline_n = ((max_price - clamped) / range) as f32;
        } else {
            midline_n = 0.5;
        }

        if slice.len() >= 2 {
            let prev = &slice[slice.len() - 2];
            let last = &slice[slice.len() - 1];
            let eps = 1e-9;
            if last.close > prev.close + eps {
                last_move_str = "up".to_string();
            } else if last.close < prev.close - eps {
                last_move_str = "down".to_string();
            } else {
                last_move_str = "flat".to_string();
            }
        } else {
            last_move_str = "flat".to_string();
        }
    }

    app.set_candle_points(ModelRc::new(VecModel::from(candle_points_vec)));
    app.set_candle_midline(midline_n);
    app.set_last_move(SharedString::from(&last_move_str));

    let _ = (snap.last_mid, snap.last_vol);
}

#[allow(dead_code)]
fn debug_print_candle_meta(label: &str, candles: &[Candle]) {
    if candles.is_empty() {
        eprintln!("[DEBUG][{}] no candles", label);
        return;
    }
    let first = &candles[0];
    let last = &candles[candles.len().saturating_sub(1)];
    eprintln!(
        "[DEBUG][{}] candles: count={}  t_range={}..{}",
        label,
        candles.len(),
        first.t,
        last.t
    );
}

fn main() {
    let base_dir = PathBuf::from("data");
    let tickers = vec!["ETH-USD".to_string(), "BTC-USD".to_string(), "SOL-USD".to_string()];

    let core = AppCore::new(base_dir.clone(), tickers.clone());
    let core_rc = Rc::new(RefCell::new(core));

    println!("DEBUG: before AppWindow::new()");
    let app = AppWindow::new().unwrap();
    println!("DEBUG: after AppWindow::new()");

    app.set_mode(SharedString::from("Live"));
    app.set_time_mode(SharedString::from("Local"));
    app.set_show_depth(true);
    app.set_show_ladders(true);
    app.set_show_trades(true);
    app.set_show_volume(true);
    app.set_trade_side(SharedString::from("Buy"));
    app.set_trade_size(0.01);
    app.set_trade_leverage(5.0);
    app.set_bot_signal(SharedString::from("none"));
    app.set_bot_size(0.0);
    app.set_bot_comment(SharedString::from(""));
    app.set_bot_auto_trade(false);
    app.set_balance_usdc(1000.0);
    app.set_balance_pnl(0.0);
    app.set_candle_midline(0.5);
    app.set_last_move(SharedString::from("flat"));

    {
        let core = core_rc.borrow();
        app.set_candle_tf_secs(core.tf_secs as i32);
        app.set_candle_window_minutes((core.window_secs / 60) as i32);
        app.set_dom_depth_levels(core.dom_depth_levels() as i32);
    }

    let default_script = r#"// Rhai bot script.
// Inputs:
//   ticker:             String
//   best_bid, best_ask, mid, spread: f64
//   bid_liquidity_near, ask_liquidity_near: f64
//   tf_secs: i64
//
// Outputs you must set:
//   bot_signal = "none" | "buy" | "sell"
//   bot_size   = positive float (units)
//   bot_comment = String

let imbalance = if ask_liquidity_near > 0.0 {
    bid_liquidity_near / ask_liquidity_near
} else {
    0.0
};

bot_signal = "none";
bot_size = 0.0;
bot_comment = "";

if imbalance > 2.5 && spread < mid * 0.0005 {
    bot_signal = "buy";
    bot_size = 0.01;
    bot_comment = "Bid bubble detected";
} else if imbalance < 0.4 && spread < mid * 0.0005 {
    bot_signal = "sell";
    bot_size = 0.01;
    bot_comment = "Ask bubble detected";
}
"#;
    app.set_script_text(SharedString::from(default_script));
    app.set_script_error(SharedString::from(""));

    let app_weak = app.as_weak();

    {
        let mut core = core_rc.borrow_mut();
        if let Some((snap, metrics)) = core.snapshot_for_ui() {
            if let Some((min_ts, max_ts)) = core.ticker_range(&core.current_ticker) {
                let range_str = format!(
                    "Range: {} -> {}",
                    format_ts_local(min_ts),
                    format_ts_local(max_ts)
                );
                app.set_data_range(SharedString::from(range_str));
            }
            apply_snapshot_to_ui(&app, &snap, &metrics, core.dom_depth_levels());
        }
        app.set_current_ticker(SharedString::from(&core.current_ticker));
    }

    {
        let core_rc_ticker = core_rc.clone();
        let app_weak_ticker = app_weak.clone();
        app.on_ticker_changed(move |new_ticker| {
            if let Some(app) = app_weak_ticker.upgrade() {
                let mut core = core_rc_ticker.borrow_mut();
                let nt = new_ticker.to_string();

                if !core.tickers.contains(&nt) {
                    eprintln!(
                        "[TICKER] requested unknown ticker {}, keeping {}",
                        nt, core.current_ticker
                    );
                    app.set_current_ticker(SharedString::from(&core.current_ticker));
                    return;
                }

                core.current_ticker = nt.clone();
                core.mark_snapshot_dirty();

                if let Some((min_ts, max_ts)) = core.ticker_range(&core.current_ticker) {
                    let range_str = format!(
                        "Range: {} -> {}",
                        format_ts_local(min_ts),
                        format_ts_local(max_ts)
                    );
                    app.set_data_range(SharedString::from(range_str));
                }

                if let Some((snap, metrics)) = core.snapshot_for_ui() {
                    apply_snapshot_to_ui(&app, &snap, &metrics, core.dom_depth_levels());
                }

                println!("[TICKER] Changed to: {}", core.current_ticker);
            }
        });
    }

    {
        let app_weak_mode = app_weak.clone();
        app.on_mode_changed(move |new_mode| {
            if let Some(app) = app_weak_mode.upgrade() {
                println!("[MODE] Changed to: {}", new_mode);
                app.set_mode(new_mode);
            }
        });
    }

    {
        let app_weak_tm = app_weak.clone();
        app.on_time_mode_changed(move |new_tm| {
            if let Some(app) = app_weak_tm.upgrade() {
                println!("[TIME MODE] Changed to: {}", new_tm);
                app.set_time_mode(new_tm);
            }
        });
    }

    {
        let app_weak_tf = app_weak.clone();
        let core_rc_tf = core_rc.clone();
        app.on_candle_tf_changed(move |new_tf| {
            if let Some(app) = app_weak_tf.upgrade() {
                let mut core = core_rc_tf.borrow_mut();
                core.set_tf_from_ui(new_tf as u64);
                app.set_candle_tf_secs(new_tf);
                if let Some((snap, metrics)) = core.snapshot_for_ui() {
                    apply_snapshot_to_ui(&app, &snap, &metrics, core.dom_depth_levels());
                }
            }
        });
    }

    {
        let app_weak_win = app_weak.clone();
        let core_rc_win = core_rc.clone();
        app.on_candle_window_changed(move |new_window_minutes| {
            if let Some(app) = app_weak_win.upgrade() {
                let mut core = core_rc_win.borrow_mut();
                core.set_window_from_ui(new_window_minutes as u64);
                app.set_candle_window_minutes(new_window_minutes);
                if let Some((snap, metrics)) = core.snapshot_for_ui() {
                    apply_snapshot_to_ui(&app, &snap, &metrics, core.dom_depth_levels());
                }
            }
        });
    }

    {
        let app_weak_dom = app_weak.clone();
        let core_rc_dom = core_rc.clone();
        app.on_dom_depth_changed(move |new_depth| {
            if let Some(app) = app_weak_dom.upgrade() {
                let mut core = core_rc_dom.borrow_mut();
                core.set_dom_depth_from_ui(new_depth);
                app.set_dom_depth_levels(new_depth);
                if let Some((snap, metrics)) = core.snapshot_for_ui() {
                    apply_snapshot_to_ui(&app, &snap, &metrics, core.dom_depth_levels());
                }
            }
        });
    }

    {
        let app_weak_send = app_weak.clone();
        let core_rc_send = core_rc.clone();
        app.on_send_order(move || {
            if let Some(app) = app_weak_send.upgrade() {
                let mut core = core_rc_send.borrow_mut();
                let side = app.get_trade_side().to_string();
                let size = app.get_trade_size();
                let size_str = format!("{:.8}", size);
                let ticker = core.current_ticker.clone();

                append_trade_csv(&core.base_dir, &ticker, "gui_manual", &side, &size_str);

                let msg = format!("Order sent: {} {} units on {}", side, size_str, ticker);
                app.set_order_message(SharedString::from(&msg));

                let receipt = Receipt {
                    ts: SharedString::from(format_ts_local(now_unix())),
                    ticker: SharedString::from(&ticker),
                    side: SharedString::from(&side),
                    kind: SharedString::from("Manual"),
                    size: SharedString::from(&size_str),
                    status: SharedString::from("submitted"),
                    comment: SharedString::from("GUI manual"),
                };
                core.push_receipt(&app, receipt);

                if let Some((_, metrics)) = core.snapshot_for_ui() {
                    println!("[ORDER] {} (mid {:.2}, spread {:.5})", msg, metrics.mid, metrics.spread);
                } else {
                    println!("[ORDER] {}", msg);
                }
            }
        });
    }

    {
        let app_weak_reload = app_weak.clone();
        let core_rc_reload = core_rc.clone();
        app.on_reload_data(move || {
            if let Some(app) = app_weak_reload.upgrade() {
                let mut core = core_rc_reload.borrow_mut();
                core.reload_current_ticker();
                if let Some((snap, metrics)) = core.snapshot_for_ui() {
                    apply_snapshot_to_ui(&app, &snap, &metrics, core.dom_depth_levels());
                }
                app.set_order_message(SharedString::from("Data reloaded"));
            }
        });
    }

    {
        let app_weak_script = app_weak.clone();
        let core_rc_script = core_rc.clone();
        app.on_run_script(move || {
            if let Some(app) = app_weak_script.upgrade() {
                let mut core = core_rc_script.borrow_mut();
                if let Some((snap, metrics)) = core.snapshot_for_ui() {
                    core.run_bot_script(&app, &metrics);
                    core.maybe_auto_trade(&app, &metrics);
                    apply_snapshot_to_ui(&app, &snap, &metrics, core.dom_depth_levels());
                    println!("[SCRIPT] run complete; signal={}", core.bot_signal);
                } else {
                    app.set_script_error(SharedString::from("No snapshot available yet"));
                }
            }
        });
    }

    {
        let app_weak_dep = app_weak.clone();
        let core_rc_dep = core_rc.clone();
        app.on_deposit(move |amount| {
            if let Some(app) = app_weak_dep.upgrade() {
                let mut core = core_rc_dep.borrow_mut();
                let mut bal = app.get_balance_usdc();
                let amt = amount.max(0.0);
                bal += amt;
                app.set_balance_usdc(bal);
                let receipt = Receipt {
                    ts: SharedString::from(format_ts_local(now_unix())),
                    ticker: SharedString::from("N/A"),
                    side: SharedString::from("N/A"),
                    kind: SharedString::from("DepositSim"),
                    size: SharedString::from(format!("{:.2}", amt)),
                    status: SharedString::from("ok"),
                    comment: SharedString::from("Sim deposit"),
                };
                core.push_receipt(&app, receipt);
            }
        });

        let app_weak_wd = app_weak.clone();
        let core_rc_wd = core_rc.clone();
        app.on_withdraw(move |amount| {
            if let Some(app) = app_weak_wd.upgrade() {
                let mut core = core_rc_wd.borrow_mut();
                let mut bal = app.get_balance_usdc();
                let amt = amount.max(0.0);
                if bal >= amt {
                    bal -= amt;
                    app.set_balance_usdc(bal);
                    let receipt = Receipt {
                        ts: SharedString::from(format_ts_local(now_unix())),
                        ticker: SharedString::from("N/A"),
                        side: SharedString::from("N/A"),
                        kind: SharedString::from("WithdrawSim"),
                        size: SharedString::from(format!("{:.2}", amt)),
                        status: SharedString::from("ok"),
                        comment: SharedString::from("Sim withdraw"),
                    };
                    core.push_receipt(&app, receipt);
                } else {
                    let receipt = Receipt {
                        ts: SharedString::from(format_ts_local(now_unix())),
                        ticker: SharedString::from("N/A"),
                        side: SharedString::from("N/A"),
                        kind: SharedString::from("WithdrawSim"),
                        size: SharedString::from(format!("{:.2}", amt)),
                        status: SharedString::from("fail"),
                        comment: SharedString::from("Insufficient sim balance"),
                    };
                    core.push_receipt(&app, receipt);
                }
            }
        });
    }

    let timer = Timer::default();
    {
        let app_weak_timer = app_weak.clone();
        let core_rc_timer = core_rc.clone();
        timer.start(TimerMode::Repeated, Duration::from_secs(1), move || {
            if let Some(app) = app_weak_timer.upgrade() {
                let mut core = core_rc_timer.borrow_mut();

                if let Some((snap, metrics)) = core.snapshot_for_ui() {
                    apply_snapshot_to_ui(&app, &snap, &metrics, core.dom_depth_levels());

                    if app.get_bot_auto_trade() {
                        core.run_bot_script(&app, &metrics);
                        core.maybe_auto_trade(&app, &metrics);
                    }
                }

                let now_ts = now_unix();
                let now_str = format_ts_local(now_ts);
                app.set_current_time(SharedString::from(now_str));
            }
        });
    }

    println!("Starting Slint Trading GUI...");
    println!("Expected CSV dir (crate-relative): {}", base_dir.display());

    app.run().unwrap();
}
