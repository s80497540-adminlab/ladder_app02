use slint::SharedString;
use std::time::{SystemTime, UNIX_EPOCH};

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
        Self { mid: 0.0, best_bid: 0.0, best_ask: 0.0, spread: 0.0, imbalance: 0.0 }
    }
}

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

            metrics: Metrics::default(),
        }
    }
}

impl AppState {
    pub fn from_ui(ui: &crate::AppWindow) -> Self {
        // Pull current values out of UI (after persistence apply), to avoid hidden desync.
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

            metrics: Metrics::default(),
        }
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
    // Keep it simple for scaffold; you can swap to chrono/local/utc later
    format!("unix:{now}")
}

pub fn ss(s: impl Into<String>) -> SharedString {
    SharedString::from(s.into())
}
