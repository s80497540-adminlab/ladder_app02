use slint::SharedString;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::debug_hooks;

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
    pub dom_depth_levels: i32,

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

    pub metrics: Metrics,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            current_ticker: "ETH-USD".to_string(),
            mode: "Live".to_string(),
            time_mode: "Local".to_string(),

            candle_tf_secs: 60,
            candle_window_minutes: 60,
            dom_depth_levels: 20,

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

            metrics: Metrics::default(),
        }
    }
}

impl AppState {
    pub fn from_ui(ui: &crate::AppWindow) -> Self {
        Self {
            current_ticker: ui.get_current_ticker().to_string(),
            mode: ui.get_mode().to_string(),
            time_mode: ui.get_time_mode().to_string(),

            candle_tf_secs: ui.get_candle_tf_secs(),
            candle_window_minutes: ui.get_candle_window_minutes(),
            dom_depth_levels: ui.get_dom_depth_levels(),

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

            metrics: Metrics::default(),
        }
    }

    pub fn reset_candles(&mut self) {
        self.candles.clear();
        self.candle_points.clear();
        self.candle_midline = 0.5;
        self.candle_active_bucket = None;

        debug_hooks::log_candle_reset("explicit reset_candles call");
    }

    fn desired_candle_count(&self) -> usize {
        let tf = self.candle_tf_secs.max(1) as usize;
        let win = self.candle_window_minutes.max(1) as usize;
        let n = (win * 60) / tf;
        n.clamp(30, 600)
    }

    fn tf_secs_u64(&self) -> u64 {
        self.candle_tf_secs.max(1) as u64
    }

    fn bucket_start(ts_unix: u64, tf_secs: u64) -> u64 {
        (ts_unix / tf_secs) * tf_secs
    }

    /// ✅ Call this whenever you have a reliable mid + timestamp (BookTop).
    pub fn on_mid_tick(&mut self, ts_unix: u64, mid: f64) {
        if !mid.is_finite() || mid <= 0.0 {
            return;
        }

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

        // trim to window
        let desired = self.desired_candle_count();
        if self.candles.len() > desired {
            let extra = self.candles.len() - desired;
            self.candles.drain(0..extra);
        }

        self.rebuild_candle_points(mid);
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
            self.on_mid_tick(ts_unix, mid);
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

        if mid_for_line > 0.0 {
            self.rebuild_candle_points(mid_for_line);
        }
    }

    fn rebuild_candle_points(&mut self, mid: f64) {
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
        let w = (1.0 / n as f32).clamp(0.001, 1.0);

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

        let mid_for_line = if mid.is_finite() && mid > 0.0 {
            mid
        } else {
            (hi + lo) * 0.5
        };
        self.candle_midline = y(mid_for_line);
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
