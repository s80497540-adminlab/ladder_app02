use serde::{Deserialize, Serialize};
use slint::SharedString;
use std::collections::{HashMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{debug_hooks, feed_shared};

const MID_TICK_CACHE_VERSION: u32 = 1;
const MAX_CONDENSED_MID_TICKS: usize = 200_000;
const CONDENSED_HISTORY_WINDOW_SECS: u64 = 24 * 60 * 60;
const TAIL_READ_CHUNK_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub struct ReceiptRow {
    pub ts: String,
    pub ticker: String,
    pub side: String,
    pub kind: String,
    pub size: String,
    pub status: String,
    pub comment: String,
}

#[derive(Debug, Clone)]
pub struct TradeRow {
    pub ts: String,
    pub side: String,
    pub size: String,
    pub is_buy: bool,
}

#[derive(Debug, Clone)]
pub struct BookLevelRow {
    pub price: String,
    pub size: String,
    pub depth_ratio: f32,
    pub is_best: bool,
}

#[derive(Debug, Clone)]
pub struct Metrics {
    pub mid: f64,
    pub best_bid: f64,
    pub best_ask: f64,
    pub spread: f64,
    pub imbalance: f64,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            mid: 0.0,
            best_bid: 0.0,
            best_ask: 0.0,
            spread: 0.0,
            imbalance: 0.0,
        }
    }
}

// -------------------- Candles (state-side) --------------------

#[derive(Clone, Debug)]
pub struct Candle {
    pub ts: String, // label for the candle start (bucket)
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64, // we add trade size into this when available
}

#[derive(Clone, Debug)]
pub struct CandlePointState {
    pub x: f32,
    pub w: f32,
    pub open: f32,
    pub high: f32,
    pub low: f32,
    pub close: f32,
    pub is_up: bool,
    pub volume: f32, // 0..1
}

// -------------------- AppState --------------------

#[derive(Debug, Clone)]
pub struct AppState {
    pub current_ticker: String,
    pub mode: String,
    pub time_mode: String,

    pub candle_tf_secs: i32,
    pub candle_window_minutes: i32,
    pub candle_price_mode: String,
    pub dom_depth_levels: i32,
    pub render_all_candles: bool,
    pub session_recording: bool,
    pub session_id: String,
    pub session_start_unix: u64,
    pub session_ticks: HashMap<String, VecDeque<MidTick>>,
    pub close_after_save: bool,
    pub feed_enabled: bool,
    pub chart_enabled: bool,
    pub depth_enabled: bool,
    pub trades_enabled: bool,
    pub volume_enabled: bool,

    pub trade_side: String,
    pub trade_size: f32,
    pub trade_leverage: f32,

    pub trade_real_mode: bool,
    pub trade_real_armed: bool,
    pub trade_real_arm_phrase: String,
    pub trade_real_arm_status: String,
    pub trade_real_arm_expires_at: Option<u64>,

    pub balance_usdc: f32,
    pub balance_pnl: f32,

    pub current_time: String,
    pub order_message: String,

    // Phase-2 “normalized” view models
    pub bids: Vec<BookLevelRow>,
    pub asks: Vec<BookLevelRow>,
    pub recent_trades: Vec<TradeRow>,
    pub receipts: Vec<ReceiptRow>,

    // Candles for chart + rows
    pub candles: Vec<Candle>,
    pub candle_points: Vec<CandlePointState>,
    pub candle_midline: f32,

    // ✅ Candle builder internal state
    pub candle_active_bucket: Option<u64>,
    pub mid_ticks: VecDeque<MidTick>,
    pub candle_last_ts: HashMap<String, u64>,
    pub pending_mid_ticks: VecDeque<MidTick>,
    pub history_loading: bool,
    pub history_load_full: bool,
    pub history_total: usize,
    pub history_done: usize,
    pub history_valve_open: bool,

    pub metrics: Metrics,

    pub daemon_active: bool,
    pub daemon_status: String,

    pub perf_frame_ms_ema: f32,
    pub perf_events_ema: f32,
    pub perf_load: f32,
    pub perf_healthy: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MidTick {
    pub ts_unix: u64,
    pub mid: f64,
    #[serde(default)]
    pub bid: f64,
    #[serde(default)]
    pub ask: f64,
}

#[derive(Debug, Serialize, Deserialize)]
struct MidTickCache {
    version: u32,
    ticks: Vec<MidTick>,
}

#[derive(Debug, Serialize)]
struct SessionMeta {
    id: String,
    start_unix: u64,
}

#[derive(Debug, Serialize)]
struct SessionTickerSummary {
    ticker: String,
    ticks: usize,
    first_unix: Option<u64>,
    last_unix: Option<u64>,
}

#[derive(Debug, Serialize)]
struct SessionSummary {
    id: String,
    start_unix: u64,
    end_unix: u64,
    tickers: Vec<SessionTickerSummary>,
}

impl Default for AppState {
    fn default() -> Self {
        let session_start_unix = now_unix();
        let session_id = Self::session_id_from_unix(session_start_unix);
        Self {
            current_ticker: "ETH-USD".to_string(),
            mode: "Live".to_string(),
            time_mode: "Local".to_string(),

            candle_tf_secs: 60,
            candle_window_minutes: 60,
            candle_price_mode: "Mid".to_string(),
            dom_depth_levels: 20,
            render_all_candles: false,
            session_recording: true,
            session_id,
            session_start_unix,
            session_ticks: HashMap::new(),
            close_after_save: false,
            feed_enabled: false,
            chart_enabled: false,
            depth_enabled: true,
            trades_enabled: true,
            volume_enabled: true,

            trade_side: "Buy".to_string(),
            trade_size: 0.01,
            trade_leverage: 5.0,

            trade_real_mode: false,
            trade_real_armed: false,
            trade_real_arm_phrase: String::new(),
            trade_real_arm_status: "NOT ARMED".to_string(),
            trade_real_arm_expires_at: None,

            balance_usdc: 1000.0,
            balance_pnl: 0.0,

            current_time: String::new(),
            order_message: String::new(),

            bids: Vec::new(),
            asks: Vec::new(),
            recent_trades: Vec::new(),
            receipts: Vec::new(),

            candles: Vec::new(),
            candle_points: Vec::new(),
            candle_midline: 0.5,
            candle_active_bucket: None,
            mid_ticks: VecDeque::new(),
            candle_last_ts: HashMap::new(),
            pending_mid_ticks: VecDeque::new(),
            history_loading: false,
            history_load_full: false,
            history_total: 0,
            history_done: 0,
            history_valve_open: false,

            metrics: Metrics::default(),

            daemon_active: false,
            daemon_status: "Daemon: idle".to_string(),

            perf_frame_ms_ema: 0.0,
            perf_events_ema: 0.0,
            perf_load: 0.0,
            perf_healthy: true,
        }
    }
}

impl AppState {
    fn price_for_mode(&self, mid: f64, bid: f64, ask: f64) -> f64 {
        let bid_ok = bid.is_finite() && bid > 0.0;
        let ask_ok = ask.is_finite() && ask > 0.0;
        match self.candle_price_mode.as_str() {
            "Bid" if bid_ok => bid,
            "Ask" if ask_ok => ask,
            _ => mid,
        }
    }

    pub fn from_ui(ui: &crate::AppWindow) -> Self {
        let session_start_unix = now_unix();
        let session_id = Self::session_id_from_unix(session_start_unix);
        let state = Self {
            current_ticker: ui.get_current_ticker().to_string(),
            mode: ui.get_mode().to_string(),
            time_mode: ui.get_time_mode().to_string(),

            candle_tf_secs: ui.get_candle_tf_secs(),
            candle_window_minutes: ui.get_candle_window_minutes(),
            candle_price_mode: ui.get_candle_price_mode().to_string(),
            dom_depth_levels: ui.get_dom_depth_levels(),
            render_all_candles: ui.get_render_all_candles(),
            session_recording: ui.get_session_recording(),
            session_id,
            session_start_unix,
            session_ticks: HashMap::new(),
            close_after_save: false,
            feed_enabled: ui.get_feed_enabled(),
            chart_enabled: ui.get_chart_enabled(),
            depth_enabled: ui.get_show_depth(),
            trades_enabled: ui.get_show_trades(),
            volume_enabled: ui.get_show_volume(),

            trade_side: ui.get_trade_side().to_string(),
            trade_size: ui.get_trade_size(),
            trade_leverage: ui.get_trade_leverage(),

            trade_real_mode: ui.get_trade_real_mode(),
            trade_real_armed: ui.get_trade_real_armed(),
            trade_real_arm_phrase: ui.get_trade_real_arm_phrase().to_string(),
            trade_real_arm_status: ui.get_trade_real_arm_status().to_string(),
            trade_real_arm_expires_at: None,

            balance_usdc: ui.get_balance_usdc(),
            balance_pnl: ui.get_balance_pnl(),

            current_time: ui.get_current_time().to_string(),
            order_message: ui.get_order_message().to_string(),

            bids: Vec::new(),
            asks: Vec::new(),
            recent_trades: Vec::new(),
            receipts: Vec::new(),

            candles: Vec::new(),
            candle_points: Vec::new(),
            candle_midline: 0.5,
            candle_active_bucket: None,
            mid_ticks: VecDeque::new(),
            candle_last_ts: HashMap::new(),
            pending_mid_ticks: VecDeque::new(),
            history_loading: false,
            history_load_full: false,
            history_total: 0,
            history_done: 0,
            history_valve_open: ui.get_history_valve_open(),

            metrics: Metrics::default(),

            daemon_active: false,
            daemon_status: "Daemon: idle".to_string(),

            perf_frame_ms_ema: 0.0,
            perf_events_ema: 0.0,
            perf_load: 0.0,
            perf_healthy: true,
        };
        state.ensure_session_dir();
        state
    }

    pub fn reset_candles(&mut self) {
        self.candles.clear();
        self.candle_points.clear();
        self.candle_midline = 0.5;
        self.candle_active_bucket = None;

        debug_hooks::log_candle_reset("explicit reset_candles call");
    }

    fn tf_secs_u64(&self) -> u64 {
        self.candle_tf_secs.max(1) as u64
    }

    fn bucket_start(ts_unix: u64, tf_secs: u64) -> u64 {
        (ts_unix / tf_secs) * tf_secs
    }

    /// ✅ Call this whenever you have a reliable mid + timestamp (BookTop).
    pub fn on_mid_tick(&mut self, ts_unix: u64, mid: f64, bid: f64, ask: f64) {
        if !mid.is_finite() || mid <= 0.0 {
            return;
        }

        let mut bid = bid;
        let mut ask = ask;
        if !bid.is_finite() || bid <= 0.0 {
            bid = mid;
        }
        if !ask.is_finite() || ask <= 0.0 {
            ask = mid;
        }
        if bid > ask {
            std::mem::swap(&mut bid, &mut ask);
        }

        self.record_mid_tick(ts_unix, mid, bid, ask);
        let price = self.price_for_mode(mid, bid, ask);
        self.apply_mid_tick(ts_unix, price);
        let ticker = self.current_ticker.clone();
        self.persist_mid_tick_for_ticker(&ticker, ts_unix, mid, bid, ask);
    }

    fn record_mid_tick(&mut self, ts_unix: u64, mid: f64, bid: f64, ask: f64) {
        self.mid_ticks.push_back(MidTick { ts_unix, mid, bid, ask });
        if !self.render_all_candles {
            let cutoff = ts_unix.saturating_sub(CONDENSED_HISTORY_WINDOW_SECS);
            while self.mid_ticks.front().map(|t| t.ts_unix < cutoff).unwrap_or(false) {
                self.mid_ticks.pop_front();
            }
            while self.mid_ticks.len() > MAX_CONDENSED_MID_TICKS {
                self.mid_ticks.pop_front();
            }
        }
    }

    fn apply_mid_tick(&mut self, ts_unix: u64, mid: f64) {
        let tf = self.tf_secs_u64();
        let bucket = Self::bucket_start(ts_unix, tf);

        match self.candle_active_bucket {
            None => {
                self.candle_active_bucket = Some(bucket);
                self.candles.push(Candle {
                    ts: format!("unix:{bucket}"),
                    open: mid,
                    high: mid,
                    low: mid,
                    close: mid,
                    volume: 0.0,
                });
                debug_hooks::log_mid_bucket(ts_unix, bucket, mid, self.candles.len());
            }
            Some(active) if active == bucket => {
                // update current candle
                if let Some(last) = self.candles.last_mut() {
                    last.close = mid;
                    if mid > last.high {
                        last.high = mid;
                    }
                    if mid < last.low {
                        last.low = mid;
                    }
                }
            }
            Some(active) if bucket > active => {
                // roll forward; fill gaps with flat candles using previous close
                let mut prev_close = self.candles.last().map(|c| c.close).unwrap_or(mid);

                let mut b = active + tf;
                while b < bucket {
                    self.candles.push(Candle {
                        ts: format!("unix:{b}"),
                        open: prev_close,
                        high: prev_close,
                        low: prev_close,
                        close: prev_close,
                        volume: 0.0,
                    });
                    debug_hooks::log_candle_gap(active, b);
                    prev_close = prev_close;
                    b += tf;
                }

                // start new active candle
                self.candle_active_bucket = Some(bucket);
                self.candles.push(Candle {
                    ts: format!("unix:{bucket}"),
                    open: prev_close,
                    high: mid.max(prev_close),
                    low: mid.min(prev_close),
                    close: mid,
                    volume: 0.0,
                });
                debug_hooks::log_mid_bucket(ts_unix, bucket, mid, self.candles.len());
            }
            Some(_) => {
                // out-of-order tick; ignore for now
                return;
            }
        }

        if self.render_all_candles && !self.history_loading {
            self.rebuild_candle_points(mid);
        }
    }

    pub fn rebuild_candles_from_history(&mut self) {
        if self.mid_ticks.is_empty() {
            self.load_mid_cache();
        }

        let mut ticks: Vec<MidTick> = self.mid_ticks.iter().cloned().collect();

        if ticks.len() < 2 && !self.candles.is_empty() {
            // Fallback: reuse existing candles to seed history if we lack mids.
            for c in &self.candles {
                if let Some(ts) = Self::parse_unix_ts(&c.ts) {
                    ticks.push(MidTick {
                        ts_unix: ts,
                        mid: c.close,
                        bid: c.close,
                        ask: c.close,
                    });
                }
            }
            ticks.sort_by_key(|t| t.ts_unix);
        }

        if ticks.is_empty() {
            return;
        }

        self.reset_candles();

        let render_points = self.render_all_candles;
        if render_points {
            self.render_all_candles = false;
        }

        for tick in ticks {
            if tick.mid > 0.0 && tick.mid.is_finite() {
                let price = self.price_for_mode(tick.mid, tick.bid, tick.ask);
                self.apply_mid_tick(tick.ts_unix, price);
            }
        }

        if render_points {
            self.render_all_candles = true;
            self.rebuild_candle_points_full();
        }
    }

    pub fn process_pending_history(&mut self, batch: usize) -> bool {
        if !self.history_valve_open {
            return false;
        }
        if self.pending_mid_ticks.is_empty() {
            if self.history_loading && self.history_total > 0 {
                self.history_loading = false;
                if self.history_load_full {
                    self.rebuild_candle_points_full();
                }
                if let Some(last) = self.mid_ticks.back() {
                    let entry = self
                        .candle_last_ts
                        .entry(self.current_ticker.clone())
                        .or_insert(0);
                    if last.ts_unix > *entry {
                        *entry = last.ts_unix;
                    }
                }
                self.order_message = "History loaded.".to_string();
                self.history_total = 0;
                self.history_done = 0;
                return true;
            }
            return false;
        }

        self.history_loading = true;
        let mut processed = 0usize;
        let mut changed = false;
        while processed < batch {
            let Some(tick) = self.pending_mid_ticks.pop_front() else {
                break;
            };
            if tick.mid > 0.0 && tick.mid.is_finite() {
                self.record_mid_tick(tick.ts_unix, tick.mid, tick.bid, tick.ask);
                let price = self.price_for_mode(tick.mid, tick.bid, tick.ask);
                self.apply_mid_tick(tick.ts_unix, price);
                changed = true;
            }
            processed += 1;
        }
        if processed > 0 {
            self.history_done = self.history_done.saturating_add(processed);
        }

        if self.pending_mid_ticks.is_empty() {
            if let Some(last) = self.mid_ticks.back() {
                let entry = self
                    .candle_last_ts
                    .entry(self.current_ticker.clone())
                    .or_insert(0);
                if last.ts_unix > *entry {
                    *entry = last.ts_unix;
                }
            }
            if self.history_load_full {
                self.rebuild_candle_points_full();
            }
            self.history_loading = false;
            self.order_message = "History loaded.".to_string();
            self.history_total = 0;
            self.history_done = 0;
        }

        changed
    }

    pub fn rebuild_candle_points_full(&mut self) {
        let mid_for_line = if self.metrics.mid.is_finite() && self.metrics.mid > 0.0 {
            self.metrics.mid
        } else {
            self.candles.last().map(|c| c.close).unwrap_or(0.0)
        };

        if mid_for_line > 0.0 {
            self.rebuild_candle_points(mid_for_line);
        } else {
            self.candle_points.clear();
            self.candle_midline = 0.5;
        }
    }

    pub fn persist_mid_tick_for_ticker(&mut self, ticker: &str, ts_unix: u64, mid: f64, bid: f64, ask: f64) {
        if ticker.is_empty() || !mid.is_finite() || mid <= 0.0 {
            return;
        }
        let Some(path) = Self::mid_log_path(ticker) else {
            return;
        };
        let mut bid = bid;
        let mut ask = ask;
        if !bid.is_finite() || bid <= 0.0 {
            bid = mid;
        }
        if !ask.is_finite() || ask <= 0.0 {
            ask = mid;
        }
        if bid > ask {
            std::mem::swap(&mut bid, &mut ask);
        }
        let tick = MidTick { ts_unix, mid, bid, ask };
        if let Ok(line) = serde_json::to_string(&tick) {
            if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
                let _ = writeln!(f, "{line}");
            }
        }
        self.record_session_tick(ticker, &tick);
        let entry = self.candle_last_ts.entry(ticker.to_string()).or_insert(0);
        if ts_unix > *entry {
            *entry = ts_unix;
        }
    }

    fn mid_log_path(ticker: &str) -> Option<PathBuf> {
        let dir = PathBuf::from(feed_shared::DATA_DIR);
        let mut safe = ticker.to_string();
        safe = safe
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        Some(dir.join(format!("candle_history_{safe}.jsonl")))
    }

    fn mid_cache_path_json(ticker: &str) -> Option<PathBuf> {
        let dir = PathBuf::from(feed_shared::DATA_DIR);
        let mut safe = ticker.to_string();
        safe = safe
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        Some(dir.join(format!("candle_history_{safe}.json")))
    }

    fn session_id_from_unix(ts_unix: u64) -> String {
        use chrono::{TimeZone, Utc};
        let ts = ts_unix.min(i64::MAX as u64) as i64;
        if let Some(dt) = Utc.timestamp_opt(ts, 0).single() {
            format!("session_{}", dt.format("%Y%m%d_%H%M%S"))
        } else {
            format!("session_{ts_unix}")
        }
    }

    fn session_root_dir() -> PathBuf {
        PathBuf::from(feed_shared::DATA_DIR).join("sessions")
    }

    fn session_dir(&self) -> PathBuf {
        Self::session_root_dir().join(&self.session_id)
    }

    pub(crate) fn ensure_session_dir(&self) {
        let dir = self.session_dir();
        if fs::create_dir_all(&dir).is_ok() {
            let meta_path = dir.join("session_meta.json");
            if !meta_path.exists() {
                let meta = SessionMeta {
                    id: self.session_id.clone(),
                    start_unix: self.session_start_unix,
                };
                if let Ok(raw) = serde_json::to_string_pretty(&meta) {
                    let _ = fs::write(meta_path, raw);
                }
            }
        }
    }

    fn session_log_path(&self, ticker: &str) -> Option<PathBuf> {
        if ticker.is_empty() {
            return None;
        }
        let mut safe = ticker.to_string();
        safe = safe
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        Some(self.session_dir().join(format!("ticks_{safe}.jsonl")))
    }

    fn record_session_tick(&mut self, ticker: &str, tick: &MidTick) {
        if !self.session_recording {
            return;
        }
        self.ensure_session_dir();
        if let Some(path) = self.session_log_path(ticker) {
            if let Ok(line) = serde_json::to_string(tick) {
                if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
                    let _ = writeln!(f, "{line}");
                }
            }
        }
        let entry = self
            .session_ticks
            .entry(ticker.to_string())
            .or_insert_with(VecDeque::new);
        entry.push_back(tick.clone());
    }

    pub fn session_ticks_for_view(&self, ticker: &str, full: bool) -> Vec<MidTick> {
        let Some(ticks) = self.session_ticks.get(ticker) else {
            return Vec::new();
        };
        if full {
            return ticks.iter().cloned().collect();
        }
        let cutoff = now_unix().saturating_sub(CONDENSED_HISTORY_WINDOW_SECS);
        let mut out: Vec<MidTick> = Vec::new();
        for tick in ticks.iter().rev() {
            if tick.ts_unix < cutoff {
                break;
            }
            out.push(tick.clone());
            if out.len() >= MAX_CONDENSED_MID_TICKS {
                break;
            }
        }
        out.reverse();
        out
    }

    pub fn load_session_ticks_for_view(&mut self, full: bool) -> bool {
        let ticks = self.session_ticks_for_view(&self.current_ticker, full);
        if ticks.is_empty() {
            return false;
        }
        self.mid_ticks = ticks.into();
        true
    }

    pub fn save_session_summary(&mut self) -> Result<PathBuf, String> {
        self.ensure_session_dir();
        let dir = self.session_dir();
        let mut tickers: Vec<String> = self.session_ticks.keys().cloned().collect();
        tickers.sort();
        let mut entries = Vec::with_capacity(tickers.len());
        for ticker in tickers {
            let ticks = match self.session_ticks.get(&ticker) {
                Some(t) => t,
                None => continue,
            };
            let first_unix = ticks.front().map(|t| t.ts_unix);
            let last_unix = ticks.back().map(|t| t.ts_unix);
            entries.push(SessionTickerSummary {
                ticker,
                ticks: ticks.len(),
                first_unix,
                last_unix,
            });
        }
        let summary = SessionSummary {
            id: self.session_id.clone(),
            start_unix: self.session_start_unix,
            end_unix: now_unix(),
            tickers: entries,
        };
        let path = dir.join("session_summary.json");
        let raw = serde_json::to_string_pretty(&summary).map_err(|e| e.to_string())?;
        fs::write(&path, raw).map_err(|e| e.to_string())?;
        Ok(path)
    }

    pub fn load_mid_cache(&mut self) {
        let ticks = Self::read_mid_ticks_for_ticker(&self.current_ticker, self.render_all_candles);
        self.mid_ticks = ticks.into();
        if let Some(last) = self.mid_ticks.back() {
            let entry = self.candle_last_ts.entry(self.current_ticker.clone()).or_insert(0);
            if last.ts_unix > *entry {
                *entry = last.ts_unix;
            }
        }
    }

    pub fn read_mid_ticks_for_ticker(ticker: &str, full: bool) -> Vec<MidTick> {
        if full {
            Self::read_mid_ticks_full(ticker)
        } else {
            Self::read_mid_ticks_time_window(ticker, CONDENSED_HISTORY_WINDOW_SECS, MAX_CONDENSED_MID_TICKS)
        }
    }

    fn read_mid_ticks_time_window(ticker: &str, window_secs: u64, max_lines: usize) -> Vec<MidTick> {
        let log_path = match Self::mid_log_path(ticker) {
            Some(p) => p,
            None => return Vec::new(),
        };

        if log_path.exists() {
            let cutoff = now_unix().saturating_sub(window_secs);
            let lines = Self::read_tail_lines_since(&log_path, cutoff, max_lines);
            let mut out = Vec::with_capacity(lines.len());
            for line in lines {
                if let Ok(tick) = serde_json::from_str::<MidTick>(&line) {
                    out.push(tick);
                }
            }
            return out;
        }

        Self::read_mid_ticks_full(ticker)
    }

    fn read_mid_ticks_full(ticker: &str) -> Vec<MidTick> {
        let log_path = match Self::mid_log_path(ticker) {
            Some(p) => p,
            None => return Vec::new(),
        };

        let mut out = Vec::new();
        if log_path.exists() {
            if let Ok(file) = OpenOptions::new().read(true).open(&log_path) {
                let reader = BufReader::new(file);
                for line in reader.lines().flatten() {
                    if let Ok(tick) = serde_json::from_str::<MidTick>(&line) {
                        out.push(tick);
                    }
                }
            }
            return out;
        }

        let path = match Self::mid_cache_path_json(ticker) {
            Some(p) => p,
            None => return Vec::new(),
        };

        let raw = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };

        if let Ok(cache) = serde_json::from_str::<MidTickCache>(&raw) {
            if cache.version == MID_TICK_CACHE_VERSION {
                return cache.ticks;
            }
        }
        Vec::new()
    }

    fn read_tail_lines_since(path: &PathBuf, cutoff_ts: u64, max_lines: usize) -> Vec<String> {
        if max_lines == 0 {
            return Vec::new();
        }

        let mut file = match File::open(path) {
            Ok(f) => f,
            Err(_) => return Vec::new(),
        };
        let mut pos = match file.metadata().map(|m| m.len()) {
            Ok(len) => len as i64,
            Err(_) => return Vec::new(),
        };

        let mut carry: Vec<u8> = Vec::new();
        let mut lines_rev: Vec<String> = Vec::new();
        let mut done = false;

        while pos > 0 && !done {
            let read_size = TAIL_READ_CHUNK_BYTES.min(pos as usize);
            pos -= read_size as i64;
            if file.seek(SeekFrom::Start(pos as u64)).is_err() {
                break;
            }
            let mut chunk = vec![0u8; read_size];
            if file.read_exact(&mut chunk).is_err() {
                break;
            }
            if !carry.is_empty() {
                chunk.extend_from_slice(&carry);
                carry.clear();
            }

            let mut parts: Vec<&[u8]> = chunk.split(|b| *b == b'\n').collect();
            if parts.is_empty() {
                continue;
            }

            if pos > 0 {
                carry = parts.remove(0).to_vec();
            }

            for part in parts.iter().rev() {
                if part.is_empty() {
                    continue;
                }
                let line = String::from_utf8_lossy(part).to_string();
                if let Some(ts) = Self::parse_mid_tick_ts(&line) {
                    if ts < cutoff_ts {
                        done = true;
                        break;
                    }
                }
                lines_rev.push(line);
                if lines_rev.len() >= max_lines {
                    done = true;
                    break;
                }
            }
        }

        if !done && pos == 0 && !carry.is_empty() && lines_rev.len() < max_lines {
            let line = String::from_utf8_lossy(&carry).to_string();
            if !line.is_empty() {
                if let Some(ts) = Self::parse_mid_tick_ts(&line) {
                    if ts >= cutoff_ts {
                        lines_rev.push(line);
                    }
                }
            }
        }

        lines_rev.reverse();
        lines_rev
    }

    fn parse_mid_tick_ts(line: &str) -> Option<u64> {
        let key = b"\"ts_unix\":";
        let bytes = line.as_bytes();
        let mut idx = None;
        for i in 0..bytes.len().saturating_sub(key.len()) {
            if &bytes[i..i + key.len()] == key {
                idx = Some(i + key.len());
                break;
            }
        }
        let mut i = idx?;
        while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
            i += 1;
        }
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if start == i {
            return None;
        }
        let num = std::str::from_utf8(&bytes[start..i]).ok()?;
        num.parse::<u64>().ok()
    }

    pub fn candle_feed_status(&self) -> String {
        if !self.feed_enabled {
            return "Feed: OFF".to_string();
        }
        let now = now_unix();
        let mut order: Vec<String> = Vec::new();
        let preferred = ["ETH-USD", "BTC-USD", "SOL-USD"];
        for tk in preferred.iter() {
            if self.candle_last_ts.contains_key(*tk) || self.current_ticker == *tk {
                order.push((*tk).to_string());
            }
        }
        let mut extra: Vec<String> = self
            .candle_last_ts
            .keys()
            .filter(|k| !order.iter().any(|o| o == *k))
            .cloned()
            .collect();
        extra.sort();
        order.extend(extra);
        if !order.iter().any(|t| t == &self.current_ticker) {
            order.insert(0, self.current_ticker.clone());
        }
        if order.is_empty() {
            return "Feed: waiting...".to_string();
        }
        let mut parts = Vec::new();
        for tk in order {
            let last = *self.candle_last_ts.get(&tk).unwrap_or(&0);
            let age = if last > 0 && now >= last {
                format!("{}s", now - last)
            } else {
                "n/a".to_string()
            };
            if tk == self.current_ticker {
                parts.push(format!("{tk} {age}"));
            } else {
                parts.push(format!("{tk} bg {age}"));
            }
        }
        format!("Feed: {}", parts.join(" | "))
    }

    pub fn update_perf(&mut self, frame_ms: f32, events: usize) -> bool {
        let alpha = 0.15;
        let mut changed = false;
        if self.perf_frame_ms_ema <= 0.0 {
            self.perf_frame_ms_ema = frame_ms.max(0.0);
            self.perf_events_ema = events as f32;
            changed = true;
        } else {
            let next_frame = self.perf_frame_ms_ema * (1.0 - alpha) + frame_ms * alpha;
            let next_events = self.perf_events_ema * (1.0 - alpha) + events as f32 * alpha;
            if (next_frame - self.perf_frame_ms_ema).abs() > 0.2
                || (next_events - self.perf_events_ema).abs() > 0.5
            {
                changed = true;
            }
            self.perf_frame_ms_ema = next_frame;
            self.perf_events_ema = next_events;
        }

        let load = (self.perf_frame_ms_ema / 33.0).min(1.0);
        if (load - self.perf_load).abs() > 0.02 {
            changed = true;
        }
        self.perf_load = load;

        let healthy = self.perf_frame_ms_ema <= 33.0 && self.perf_events_ema <= 200.0;
        if healthy != self.perf_healthy {
            changed = true;
        }
        self.perf_healthy = healthy;

        changed
    }

    pub fn perf_status(&self) -> String {
        if self.perf_frame_ms_ema <= 0.0 {
            return "Perf: idle".to_string();
        }
        format!(
            "Perf: {:.0}ms | {:.0} ev",
            self.perf_frame_ms_ema, self.perf_events_ema
        )
    }

    pub fn update_daemon_status(&mut self, now_unix: u64) -> bool {
        let path = feed_shared::event_log_path();
        let (active, status) = if !path.exists() {
            (false, "Daemon: no log".to_string())
        } else {
            let mtime = fs::metadata(&path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs());
            match mtime {
                Some(ts) => {
                    let age = now_unix.saturating_sub(ts);
                    let active = age <= 5;
                    let status = if active {
                        format!("Daemon: writing ({age}s)")
                    } else {
                        format!("Daemon: idle ({age}s)")
                    };
                    (active, status)
                }
                None => (false, "Daemon: unknown".to_string()),
            }
        };

        let mut changed = false;
        if active != self.daemon_active {
            self.daemon_active = active;
            changed = true;
        }
        if status != self.daemon_status {
            self.daemon_status = status;
            changed = true;
        }
        changed
    }

    pub fn history_status(&self) -> String {
        if !self.history_valve_open {
            if self.history_total > 0 || !self.pending_mid_ticks.is_empty() || self.history_loading {
                return "History: paused (valve closed)".to_string();
            }
            return "History: valve closed".to_string();
        }
        if !self.history_loading {
            return String::new();
        }
        if self.history_total == 0 {
            return "History: loading...".to_string();
        }
        let done = self.history_done.min(self.history_total);
        let pct = ((done as f64 * 100.0) / self.history_total as f64).round() as i32;
        format!("History: {pct}% ({done}/{})", self.history_total)
    }

    fn parse_unix_ts(ts: &str) -> Option<u64> {
        ts.strip_prefix("unix:")?.parse::<u64>().ok()
    }

    /// ✅ Call this on Trade events to add volume into the most recent candle.
    pub fn on_trade_volume(&mut self, ts_unix: u64, trade_size: f64) {
        if trade_size <= 0.0 || !trade_size.is_finite() {
            return;
        }

        // Ensure candle exists for this time (uses current mid if available)
        let mid = if self.metrics.mid.is_finite() && self.metrics.mid > 0.0 {
            self.metrics.mid
        } else {
            self.candles.last().map(|c| c.close).unwrap_or(0.0)
        };

        if mid > 0.0 {
            self.apply_mid_tick(ts_unix, mid);
        }

        if let Some(last) = self.candles.last_mut() {
            last.volume += trade_size;
            debug_hooks::log_candle_volume(ts_unix, trade_size, Some(last.ts.clone()));
        }

        let mid_for_line = if self.metrics.mid.is_finite() && self.metrics.mid > 0.0 {
            self.metrics.mid
        } else {
            self.candles.last().map(|c| c.close).unwrap_or(0.0)
        };

        if self.render_all_candles && mid_for_line > 0.0 {
            self.rebuild_candle_points(mid_for_line);
        }
    }

    fn rebuild_candle_points(&mut self, _mid: f64) {
        if self.candles.is_empty() {
            self.candle_points.clear();
            self.candle_midline = 0.5;
            return;
        }

        let mut lo = f64::INFINITY;
        let mut hi = f64::NEG_INFINITY;
        let mut vmax: f64 = 0.0; // ✅ explicit type

        for c in &self.candles {
            lo = lo.min(c.low);
            hi = hi.max(c.high);
            vmax = vmax.max(c.volume);
        }

        // pad
        let mut span = hi - lo;
        if !span.is_finite() || span <= 0.0 {
            span = hi.abs().max(1.0);
            lo = hi - span;
        }
        let pad = span * 0.02;
        lo -= pad;
        hi += pad;
        let span = (hi - lo).max(1e-9);

        // 0 = top, 1 = bottom
        let y = |price: f64| -> f32 { ((hi - price) / span).clamp(0.0, 1.0) as f32 };

        let n = self.candles.len().max(1);
        let w = (1.0 / n as f32).clamp(0.01, 0.2);

        self.candle_points = self
            .candles
            .iter()
            .enumerate()
            .map(|(i, c)| CandlePointState {
                x: (i as f32 + 0.5) / n as f32,
                w,
                open: y(c.open),
                high: y(c.high),
                low: y(c.low),
                close: y(c.close),
                is_up: c.close >= c.open,
                volume: if vmax > 0.0 {
                    (c.volume / vmax).clamp(0.0, 1.0) as f32
                } else {
                    0.0
                },
            })
            .collect();

        // Keep midline visually centered to avoid vertical drift as prices move.
        self.candle_midline = 0.5;
    }
}

/// unix seconds
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn format_time_basic(now: u64) -> String {
    format!("unix:{now}")
}

pub fn ss(s: impl Into<String>) -> SharedString {
    SharedString::from(s.into())
}
