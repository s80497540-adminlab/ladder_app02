mod candle_agg;
mod settings;
mod signer;

slint::include_modules!();

use crate::candle_agg::{Candle, CandleAgg};
use crate::settings::{Network, SettingsManager};
use crate::signer::{SignRequest, SignerError, SignerManager};

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

// ---- Signer/UI helpers -----------------------------------------------------

fn signer_err_to_string(e: &SignerError) -> String {
    match e {
        SignerError::WalletNotConnected => "Connect wallet first.".to_string(),
        SignerError::AutoSignDisabled => "Enable Auto-sign first.".to_string(),
        SignerError::NoActiveSession => "Create a signing session.".to_string(),
        SignerError::SessionExpired => "Signing session expired. Create a new one.".to_string(),
        SignerError::InvalidRequest(msg) => format!("Invalid sign request: {msg}"),
    }
}

fn signer_status_for_ui(signer: &SignerManager, now: u64) -> String {
    let sess = signer.session_state();
    if sess.active {
        if let Some(exp) = sess.expires_at_unix {
            let mins_left = exp.saturating_sub(now) / 60;
            return format!("session active ({}m left)", mins_left);
        }
        return "session active".to_string();
    }

    match signer.can_sign(now) {
        Ok(()) => "session active".to_string(),
        Err(SignerError::WalletNotConnected) => "inactive".to_string(),
        Err(SignerError::AutoSignDisabled) => "ready (auto-sign off)".to_string(),
        Err(SignerError::NoActiveSession) => "ready (session not created)".to_string(),
        Err(SignerError::SessionExpired) => "expired".to_string(),
        Err(SignerError::InvalidRequest(_)) => "ready".to_string(),
    }
}

// ---- Settings UI helper ----------------------------------------------------

fn apply_settings_to_ui(
    app: &AppWindow,
    st: &crate::settings::SettingsState,
    signer_status: &str,
    last_error_override: Option<&str>,
) {
    app.set_settings_wallet_address(SharedString::from(&st.wallet_address));
    app.set_settings_wallet_status(SharedString::from(&st.wallet_status));

    app.set_settings_network(SharedString::from(st.network.as_str()));
    app.set_settings_rpc_endpoint(SharedString::from(&st.rpc_endpoint));

    app.set_settings_auto_sign(st.auto_sign);
    app.set_settings_session_ttl_minutes(SharedString::from(st.session_ttl_minutes.to_string()));

    app.set_settings_signer_status(SharedString::from(signer_status));

    let err = last_error_override.unwrap_or(&st.last_error);
    app.set_settings_last_error(SharedString::from(err));
}

// ---- App core --------------------------------------------------------------

struct AppCore {
    base_dir: PathBuf,
    tickers: Vec<String>,
    ticker_data: HashMap<String, TickerData>,
    current_ticker: String,
    tf_secs: u64,
    window_secs: u64,

    // 4F/4G latch
    last_real_mode: bool,

    #[allow(dead_code)]
    last_reload_ts: u64,

    #[allow(dead_code)]
    engine: Engine,
    #[allow(dead_code)]
    scope: Scope<'static>,
    #[allow(dead_code)]
    script_error: String,

    receipts: Vec<Receipt>,

    cached_snapshot: Option<Snapshot>,
    cached_metrics: Option<BubbleMetrics>,
    snapshot_dirty: bool,

    dom_depth_levels: usize,

    settings: SettingsManager,
    signer: SignerManager,
}

impl AppCore {
    fn new(base_dir: PathBuf, tickers: Vec<String>) -> Self {
        let mut ticker_data = HashMap::new();

        for tk in &tickers {
            if let Some(td) = load_ticker_data(&base_dir, tk) {
                ticker_data.insert(tk.clone(), td);
            }
        }

        let current_ticker = tickers
            .get(0)
            .cloned()
            .unwrap_or_else(|| "ETH-USD".to_string());

        let mut engine = Engine::new();
        engine.set_max_expr_depths(64, 64);

        let scope = Scope::new();

        let settings = SettingsManager::new(base_dir.clone());
        let signer = SignerManager::new();

        Self {
            base_dir,
            tickers,
            ticker_data,
            current_ticker,
            tf_secs: 60,
            window_secs: 3600,
            last_real_mode: false,
            last_reload_ts: now_unix(),
            engine,
            scope,
            script_error: String::new(),
            receipts: Vec::new(),
            cached_snapshot: None,
            cached_metrics: None,
            snapshot_dirty: true,
            dom_depth_levels: 20,
            settings,
            signer,
        }
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

    fn push_receipt(&mut self, app: &AppWindow, r: Receipt) {
        self.receipts.push(r);
        if self.receipts.len() > 300 {
            let extra = self.receipts.len() - 300;
            self.receipts.drain(0..extra);
        }
        app.set_receipts(ModelRc::new(VecModel::from(self.receipts.clone())));
    }
}

// ---- UI wiring (snapshot) --------------------------------------------------

fn apply_snapshot_to_ui(
    app: &AppWindow,
    snap: &Snapshot,
    metrics: &BubbleMetrics,
    dom_depth_levels: usize,
) {
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

    let max_bid = bid_levels_raw.iter().fold(0.0f64, |acc, (_, s)| acc.max(s.abs()));
    let max_ask = ask_levels_raw.iter().fold(0.0f64, |acc, (_, s)| acc.max(s.abs()));

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
        let mut max_vol: f64 = 0.0;

        for c in slice {
            min_price = min_price.min(c.low);
            max_price = max_price.max(c.high);
            max_vol = max_vol.max(c.volume);
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
            let w = (1.0f32 / n.max(1) as f32) * 0.7;

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
        }
    }

    app.set_candle_points(ModelRc::new(VecModel::from(candle_points_vec)));
    app.set_candle_midline(midline_n);
    app.set_last_move(SharedString::from(&last_move_str));
}

fn main() {
    let base_dir = PathBuf::from("data");
    let tickers = vec!["ETH-USD".to_string(), "BTC-USD".to_string(), "SOL-USD".to_string()];

    let core = AppCore::new(base_dir.clone(), tickers.clone());
    let core_rc = Rc::new(RefCell::new(core));

    let app = AppWindow::new().unwrap();

    // UI defaults
    app.set_mode(SharedString::from("Live"));
    app.set_time_mode(SharedString::from("Local"));
    app.set_show_depth(true);
    app.set_show_ladders(true);
    app.set_show_trades(true);
    app.set_show_volume(true);
    app.set_trade_side(SharedString::from("Buy"));
    app.set_trade_size(0.01);
    app.set_trade_leverage(5.0);

    // REAL trading toggle default OFF
    app.set_trade_real_mode(false);

    app.set_balance_usdc(1000.0);
    app.set_balance_pnl(0.0);
    app.set_candle_midline(0.5);
    app.set_last_move(SharedString::from("flat"));
    app.set_order_message(SharedString::from(""));
    app.set_receipts(ModelRc::new(VecModel::from(Vec::<Receipt>::new())));

    // Apply persisted settings into UI + init signer view
    {
        let mut core = core_rc.borrow_mut();
        let now = now_unix();

        core.settings.refresh_status(now);
        let st = core.settings.state();

        core.signer.set_wallet_connected(st.wallet_connected, now);
        if st.wallet_connected {
            let _ = core.signer.set_auto_sign_enabled(st.auto_sign, now);
        } else {
            let _ = core.signer.set_auto_sign_enabled(false, now);
        }

        core.last_real_mode = app.get_trade_real_mode();

        let signer_status = signer_status_for_ui(&core.signer, now);
        apply_settings_to_ui(&app, &st, &signer_status, None);

        if let Some((snap, metrics)) = core.snapshot_for_ui() {
            if let Some((min_ts, max_ts)) = core.ticker_range(&core.current_ticker) {
                let range_str = format!(
                    "Range: {} -> {}",
                    format_ts_local(min_ts),
                    format_ts_local(max_ts)
                );
                app.set_data_range(SharedString::from(range_str));
            }
            apply_snapshot_to_ui(&app, &snap, &metrics, core.dom_depth_levels);
        }
        app.set_current_ticker(SharedString::from(&core.current_ticker));
    }

    let app_weak = app.as_weak();

    // -----------------------------------------------------------------------
    // 4G: REAL toggle requires Mainnet + forces Live + confirmation message
    // -----------------------------------------------------------------------
    {
        let app_weak_r = app_weak.clone();
        let core_rc_r = core_rc.clone();
        app.on_trade_real_mode_toggled(move |enabled| {
            if let Some(app) = app_weak_r.upgrade() {
                let mut core = core_rc_r.borrow_mut();

                let st = core.settings.state();
                let is_mainnet = matches!(st.network, Network::Mainnet);

                // deny REAL if not mainnet
                if enabled && !is_mainnet {
                    app.set_trade_real_mode(false);
                    core.last_real_mode = false;
                    app.set_order_message(SharedString::from(
                        "REAL requires Mainnet — switch Network to Mainnet first.",
                    ));
                    return;
                }

                let was = core.last_real_mode;
                core.last_real_mode = enabled;

                if enabled {
                    // force Live
                    let mode_now = app.get_mode().to_string();
                    if !mode_now.eq_ignore_ascii_case("live") {
                        app.set_mode(SharedString::from("Live"));
                        app.set_order_message(SharedString::from(
                            "REAL enabled (Mainnet) → switched to Live.",
                        ));
                    } else if !was {
                        app.set_order_message(SharedString::from("REAL enabled (Mainnet)."));
                    }
                } else if was {
                    app.set_order_message(SharedString::from("REAL disabled."));
                }
            }
        });
    }

    // -----------------------------------------------------------------------
    // 4F/4G: Block Replay while REAL is enabled
    // -----------------------------------------------------------------------
    {
        let app_weak_m = app_weak.clone();
        let core_rc_m = core_rc.clone();
        app.on_mode_changed(move |new_mode| {
            if let Some(app) = app_weak_m.upgrade() {
                let core = core_rc_m.borrow();
                let requested = new_mode.to_string();

                if core.last_real_mode && requested.eq_ignore_ascii_case("replay") {
                    app.set_mode(SharedString::from("Live"));
                    app.set_order_message(SharedString::from(
                        "Replay is disabled while REAL is enabled — staying in Live.",
                    ));
                } else {
                    app.set_mode(SharedString::from(requested));
                }
            }
        });
    }

    // -----------------------------------------------------------------------
    // SEND ORDER gating:
    // - REAL requires Mainnet + Live + valid signing session
    // - SIM orders still write CSV
    // -----------------------------------------------------------------------
    {
        let app_weak_send = app_weak.clone();
        let core_rc_send = core_rc.clone();
        app.on_send_order(move || {
            if let Some(app) = app_weak_send.upgrade() {
                let now = now_unix();
                let mut core = core_rc_send.borrow_mut();

                let side = app.get_trade_side().to_string();
                let size = app.get_trade_size() as f64;
                let leverage = app.get_trade_leverage() as f64;
                let ticker = core.current_ticker.clone();

                let mode = app.get_mode().to_string();
                let real_mode = app.get_trade_real_mode();

                let st = core.settings.state();
                let is_mainnet = matches!(st.network, Network::Mainnet);

                // 4G hard safety: REAL cannot operate off-mainnet
                if real_mode && !is_mainnet {
                    let msg = "Blocked: REAL requires Mainnet (network mismatch).".to_string();
                    app.set_order_message(SharedString::from(&msg));

                    let receipt = Receipt {
                        ts: SharedString::from(format_ts_local(now)),
                        ticker: SharedString::from(&ticker),
                        side: SharedString::from(&side),
                        kind: SharedString::from("ManualReal"),
                        size: SharedString::from(format!("{:.8}", size)),
                        status: SharedString::from("fail"),
                        comment: SharedString::from(&msg),
                    };
                    core.push_receipt(&app, receipt);
                    return;
                }

                let requires_signing = mode.eq_ignore_ascii_case("live") && real_mode && is_mainnet;

                let mut signature_preview: Option<String> = None;

                if requires_signing {
                    let req = SignRequest {
                        ticker: ticker.clone(),
                        side: side.clone(),
                        size,
                        leverage,
                        ts_unix: now,
                    };

                    match core.signer.sign_request(&req, now) {
                        Ok(sig) => {
                            let s = sig.0;
                            let preview = if s.len() > 16 {
                                format!("{}…{}", &s[..8], &s[s.len() - 6..])
                            } else {
                                s
                            };
                            signature_preview = Some(preview);
                        }
                        Err(e) => {
                            let msg = format!("Blocked (REAL mode): {}", signer_err_to_string(&e));
                            app.set_order_message(SharedString::from(&msg));

                            let receipt = Receipt {
                                ts: SharedString::from(format_ts_local(now)),
                                ticker: SharedString::from(&ticker),
                                side: SharedString::from(&side),
                                kind: SharedString::from("ManualReal"),
                                size: SharedString::from(format!("{:.8}", size)),
                                status: SharedString::from("fail"),
                                comment: SharedString::from(&msg),
                            };
                            core.push_receipt(&app, receipt);

                            let signer_status = signer_status_for_ui(&core.signer, now);
                            apply_settings_to_ui(&app, &st, &signer_status, Some(&msg));
                            return;
                        }
                    }
                }

                let size_str = format!("{:.8}", size);

                let source = if requires_signing {
                    "gui_real_signed"
                } else if real_mode {
                    "gui_real_unverified"
                } else {
                    "gui_sim"
                };

                append_trade_csv(&core.base_dir, &ticker, source, &side, &size_str);

                let msg = if requires_signing {
                    if let Some(prev) = &signature_preview {
                        format!(
                            "Order submitted (REAL+signed): {} {} on {}  sig={}",
                            side, size_str, ticker, prev
                        )
                    } else {
                        format!("Order submitted (REAL+signed): {} {} on {}", side, size_str, ticker)
                    }
                } else if real_mode {
                    format!(
                        "Order submitted (REAL requested, not Live): {} {} on {}",
                        side, size_str, ticker
                    )
                } else {
                    format!("Order submitted (SIM): {} {} on {}", side, size_str, ticker)
                };

                app.set_order_message(SharedString::from(&msg));

                let comment = if let Some(prev) = &signature_preview {
                    format!("sig={}", prev)
                } else {
                    "no-sign".to_string()
                };

                let kind = if requires_signing {
                    "ManualRealSigned"
                } else if real_mode {
                    "ManualReal"
                } else {
                    "ManualSim"
                };

                let receipt = Receipt {
                    ts: SharedString::from(format_ts_local(now)),
                    ticker: SharedString::from(&ticker),
                    side: SharedString::from(&side),
                    kind: SharedString::from(kind),
                    size: SharedString::from(&size_str),
                    status: SharedString::from("submitted"),
                    comment: SharedString::from(comment),
                };
                core.push_receipt(&app, receipt);
            }
        });
    }

    // --- Settings callbacks ---
    {
        let app_weak_s = app_weak.clone();
        let core_rc_s = core_rc.clone();
        app.on_settings_connect_wallet(move || {
            if let Some(app) = app_weak_s.upgrade() {
                let now = now_unix();
                let mut core = core_rc_s.borrow_mut();

                let addr = app.get_settings_wallet_address().to_string();
                core.settings.connect_wallet(now, addr);

                let st = core.settings.state();
                core.signer.set_wallet_connected(st.wallet_connected, now);
                if st.wallet_connected {
                    let _ = core.signer.set_auto_sign_enabled(st.auto_sign, now);
                } else {
                    let _ = core.signer.set_auto_sign_enabled(false, now);
                }

                let signer_status = signer_status_for_ui(&core.signer, now);
                apply_settings_to_ui(&app, &st, &signer_status, None);
            }
        });
    }

    {
        let app_weak_s = app_weak.clone();
        let core_rc_s = core_rc.clone();
        app.on_settings_disconnect_wallet(move || {
            if let Some(app) = app_weak_s.upgrade() {
                let now = now_unix();
                let mut core = core_rc_s.borrow_mut();

                core.settings.disconnect_wallet(now);
                core.signer.set_wallet_connected(false, now);
                let _ = core.signer.set_auto_sign_enabled(false, now);

                let st = core.settings.state();
                let signer_status = signer_status_for_ui(&core.signer, now);
                apply_settings_to_ui(&app, &st, &signer_status, None);
            }
        });
    }

    {
        let app_weak_s = app_weak.clone();
        let core_rc_s = core_rc.clone();
        app.on_settings_refresh_status(move || {
            if let Some(app) = app_weak_s.upgrade() {
                let now = now_unix();
                let mut core = core_rc_s.borrow_mut();

                core.settings.refresh_status(now);
                let st = core.settings.state();

                core.signer.set_wallet_connected(st.wallet_connected, now);
                if st.wallet_connected {
                    let _ = core.signer.set_auto_sign_enabled(st.auto_sign, now);
                } else {
                    let _ = core.signer.set_auto_sign_enabled(false, now);
                }

                let signer_status = signer_status_for_ui(&core.signer, now);
                apply_settings_to_ui(&app, &st, &signer_status, None);
            }
        });
    }

    // ONE select_network handler, with 4G enforcement
    {
        let app_weak_s = app_weak.clone();
        let core_rc_s = core_rc.clone();
        app.on_settings_select_network(move |net| {
            if let Some(app) = app_weak_s.upgrade() {
                let now = now_unix();
                let mut core = core_rc_s.borrow_mut();

                let n = Network::from_str(&net.to_string());
                core.settings.select_network(now, n);

                let st = core.settings.state();
                let is_mainnet = matches!(st.network, Network::Mainnet);

                // 4G: if leaving Mainnet while REAL on -> auto disable REAL
                if !is_mainnet && app.get_trade_real_mode() {
                    app.set_trade_real_mode(false);
                    core.last_real_mode = false;
                    app.set_order_message(SharedString::from(
                        "Network switched off Mainnet → REAL disabled.",
                    ));
                }

                // If switching to Mainnet while REAL is already on, force Live (just in case)
                if is_mainnet && app.get_trade_real_mode() {
                    let mode_now = app.get_mode().to_string();
                    if !mode_now.eq_ignore_ascii_case("live") {
                        app.set_mode(SharedString::from("Live"));
                        app.set_order_message(SharedString::from(
                            "Mainnet + REAL → switched to Live.",
                        ));
                    }
                }

                let signer_status = signer_status_for_ui(&core.signer, now);
                apply_settings_to_ui(&app, &st, &signer_status, None);
            }
        });
    }

    {
        let app_weak_s = app_weak.clone();
        let core_rc_s = core_rc.clone();
        app.on_settings_toggle_auto_sign(move |enabled| {
            if let Some(app) = app_weak_s.upgrade() {
                let now = now_unix();
                let mut core = core_rc_s.borrow_mut();

                core.settings.toggle_auto_sign(now, enabled);
                let st = core.settings.state();

                let mut signer_err: Option<String> = None;
                core.signer.set_wallet_connected(st.wallet_connected, now);
                match core.signer.set_auto_sign_enabled(st.auto_sign, now) {
                    Ok(()) => {}
                    Err(e) => {
                        signer_err = Some(signer_err_to_string(&e));
                        let _ = core.signer.set_auto_sign_enabled(false, now);
                    }
                }

                let signer_status = signer_status_for_ui(&core.signer, now);
                apply_settings_to_ui(&app, &st, &signer_status, signer_err.as_deref());
            }
        });
    }

    {
        let app_weak_s = app_weak.clone();
        let core_rc_s = core_rc.clone();
        app.on_settings_create_session(move |ttl| {
            if let Some(app) = app_weak_s.upgrade() {
                let now = now_unix();
                let mut core = core_rc_s.borrow_mut();

                core.settings.create_session(now, ttl.to_string());
                let st = core.settings.state();

                let mut signer_err: Option<String> = None;
                core.signer.set_wallet_connected(st.wallet_connected, now);
                let _ = core.signer.set_auto_sign_enabled(st.auto_sign, now);

                match core.signer.create_session(now, st.session_ttl_minutes) {
                    Ok(_sid) => {}
                    Err(e) => signer_err = Some(signer_err_to_string(&e)),
                }

                let signer_status = signer_status_for_ui(&core.signer, now);
                apply_settings_to_ui(&app, &st, &signer_status, signer_err.as_deref());
            }
        });
    }

    {
        let app_weak_s = app_weak.clone();
        let core_rc_s = core_rc.clone();
        app.on_settings_revoke_session(move || {
            if let Some(app) = app_weak_s.upgrade() {
                let now = now_unix();
                let mut core = core_rc_s.borrow_mut();

                core.settings.revoke_session(now);
                core.signer.revoke_session();

                let st = core.settings.state();
                let signer_status = signer_status_for_ui(&core.signer, now);
                apply_settings_to_ui(&app, &st, &signer_status, None);
            }
        });
    }

    // Timer: keep current time + session expiry refresh
    let timer = Timer::default();
    {
        let app_weak_timer = app_weak.clone();
        let core_rc_timer = core_rc.clone();
        timer.start(TimerMode::Repeated, Duration::from_secs(1), move || {
            if let Some(app) = app_weak_timer.upgrade() {
                let now = now_unix();
                let mut core = core_rc_timer.borrow_mut();

                if core.signer.tick(now) {
                    let st = core.settings.state();
                    let signer_status = signer_status_for_ui(&core.signer, now);
                    apply_settings_to_ui(&app, &st, &signer_status, None);
                }

                app.set_current_time(SharedString::from(format_ts_local(now)));
            }
        });
    }

    app.run().unwrap();
}
