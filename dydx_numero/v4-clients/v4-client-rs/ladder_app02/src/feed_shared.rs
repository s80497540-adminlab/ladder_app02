use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const SNAPSHOT_FILE: &str = "dydx_live_snapshot.json";
pub const EVENT_LOG_FILE: &str = "dydx_live_feed.jsonl";

/// Get the data directory - uses executable directory on Windows, platform data dir on others
fn get_data_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        // On Windows, use the directory where the executable is located
        std::env::current_exe()
            .ok()
            .and_then(|exe_path| exe_path.parent().map(|p| p.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("data"))
    }
    #[cfg(not(target_os = "windows"))]
    {
        // On Unix-like systems, use the proper data directory
        directories::ProjectDirs::from("", "", "dydx_ladder")
            .map(|dirs| dirs.data_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from("data"))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookTopRecord {
    pub ts_unix: u64,
    pub ticker: String,
    pub best_bid: f64,
    pub best_ask: f64,
    #[serde(default)]
    pub best_bid_raw: String,
    #[serde(default)]
    pub best_ask_raw: String,
    pub bid_liq: f64,
    pub ask_liq: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookLevel {
    pub price: f64,
    pub size: f64,
    #[serde(default)]
    pub price_raw: String,
    #[serde(default)]
    pub size_raw: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookLevelsRecord {
    pub ts_unix: u64,
    pub ticker: String,
    pub bids: Vec<BookLevel>,
    pub asks: Vec<BookLevel>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeRecord {
    pub ts_unix: u64,
    pub ticker: String,
    pub side: String,
    pub size: String,
    #[serde(default)]
    pub price: f64,
    #[serde(default)]
    pub price_raw: String,
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
    get_data_dir().join(SNAPSHOT_FILE)
}

pub fn event_log_path() -> PathBuf {
    get_data_dir().join(EVENT_LOG_FILE)
}

pub fn data_dir() -> PathBuf {
    get_data_dir()
}

pub fn ensure_data_dir() -> std::io::Result<()> {
    std::fs::create_dir_all(get_data_dir())
}
