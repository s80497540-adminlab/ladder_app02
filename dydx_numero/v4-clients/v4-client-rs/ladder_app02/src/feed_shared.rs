use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const DATA_DIR: &str = "data";
pub const SNAPSHOT_FILE: &str = "dydx_live_snapshot.json";
pub const EVENT_LOG_FILE: &str = "dydx_live_feed.jsonl";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookTopRecord {
    pub ts_unix: u64,
    pub ticker: String,
    pub best_bid: f64,
    pub best_ask: f64,
    pub bid_liq: f64,
    pub ask_liq: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeRecord {
    pub ts_unix: u64,
    pub ticker: String,
    pub side: String,
    pub size: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SnapshotState {
    pub last_book: Option<BookTopRecord>,
    pub recent_trades: Vec<TradeRecord>,
}

impl SnapshotState {
    pub fn trim_trades(&mut self, max: usize) {
        if self.recent_trades.len() > max {
            let start = self.recent_trades.len() - max;
            self.recent_trades = self.recent_trades.split_off(start);
        }
    }
}

pub fn snapshot_path() -> PathBuf {
    PathBuf::from(DATA_DIR).join(SNAPSHOT_FILE)
}

pub fn event_log_path() -> PathBuf {
    PathBuf::from(DATA_DIR).join(EVENT_LOG_FILE)
}
