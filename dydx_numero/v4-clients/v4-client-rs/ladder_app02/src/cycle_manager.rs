// cycle_manager.rs - Manages 71-hour daemon cycles with preparation phases

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use serde::{Deserialize, Serialize};
use anyhow::Result;

pub const CYCLE_DURATION_SECS: u64 = 71 * 3600;  // 71 hours
pub const PREP_DURATION_SECS: u64 = 1 * 3600;   // 1 hour preparation
pub const TOTAL_CYCLE_SECS: u64 = CYCLE_DURATION_SECS + PREP_DURATION_SECS;  // 72 hours

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CycleStats {
    pub cycle_number: u64,
    pub cycle_start_unix: u64,
    pub bytes_written: u64,
    pub bytes_per_sec: f64,
    pub tickers_active: usize,
    pub in_preparation_mode: bool,
    pub prep_start_unix: Option<u64>,
    pub prev_cycle_bytes: u64,
}

impl Default for CycleStats {
    fn default() -> Self {
        Self {
            cycle_number: 1,
            cycle_start_unix: now_unix(),
            bytes_written: 0,
            bytes_per_sec: 0.0,
            tickers_active: 3,
            in_preparation_mode: false,
            prep_start_unix: None,
            prev_cycle_bytes: 0,
        }
    }
}

impl CycleStats {
    pub fn project_next_72h_bytes(&self) -> u64 {
        // Project based on current rate: bytes_per_sec × 72 hours
        let bytes_per_72h = (self.bytes_per_sec * 72.0 * 3600.0) as u64;
        bytes_per_72h.max(self.prev_cycle_bytes)  // Use max of current rate vs last cycle
    }

    pub fn project_next_72h_gb(&self) -> f64 {
        self.project_next_72h_bytes() as f64 / (1024.0 * 1024.0 * 1024.0)
    }

    pub fn secs_until_prep(&self) -> u64 {
        let elapsed = now_unix().saturating_sub(self.cycle_start_unix);
        CYCLE_DURATION_SECS.saturating_sub(elapsed)
    }

    pub fn should_enter_prep_mode(&self) -> bool {
        self.secs_until_prep() == 0 && !self.in_preparation_mode
    }

    pub fn enter_prep_mode(&mut self) {
        self.in_preparation_mode = true;
        self.prep_start_unix = Some(now_unix());
    }

    pub fn secs_until_next_cycle(&self) -> u64 {
        if !self.in_preparation_mode {
            self.secs_until_prep()
        } else {
            let prep_elapsed = now_unix().saturating_sub(self.prep_start_unix.unwrap_or(0));
            PREP_DURATION_SECS.saturating_sub(prep_elapsed)
        }
    }

    pub fn should_rotate_and_restart(&self) -> bool {
        self.in_preparation_mode && self.secs_until_next_cycle() == 0
    }

    pub fn update_rate(&mut self, bytes_written: u64, elapsed_secs: u64) {
        self.bytes_written = bytes_written;
        if elapsed_secs > 0 {
            self.bytes_per_sec = bytes_written as f64 / elapsed_secs as f64;
        }
    }
}

pub fn cycle_stats_path() -> PathBuf {
    feed_shared::data_dir().join("cycle_stats.json")
}

pub fn load_cycle_stats() -> Result<CycleStats> {
    let path = cycle_stats_path();
    if !path.exists() {
        return Ok(CycleStats::default());
    }
    let raw = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&raw)?)
}

pub fn save_cycle_stats(stats: &CycleStats) -> Result<()> {
    let path = cycle_stats_path();
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let raw = serde_json::to_string_pretty(stats)?;
    std::fs::write(path, raw)?;
    Ok(())
}

pub fn rotate_and_reset_cycle(current_stats: &CycleStats) -> Result<CycleStats> {
    // Archive current log
    let log_path = feed_shared::event_log_path();
    if log_path.exists() {
        let timestamp = now_unix();
        let archive_name = format!(
            "dydx_live_feed_cycle_{}_unix_{}.jsonl",
            current_stats.cycle_number, timestamp
        );
        let archive_path = feed_shared::data_dir().join(archive_name);
        std::fs::rename(&log_path, &archive_path)?;
        println!("[cycle_manager] archived log to {:?}", archive_path);
    }

    // Create new cycle stats
    let mut new_stats = CycleStats::default();
    new_stats.cycle_number = current_stats.cycle_number + 1;
    new_stats.cycle_start_unix = now_unix();
    new_stats.prev_cycle_bytes = current_stats.bytes_written;
    new_stats.tickers_active = current_stats.tickers_active;

    println!(
        "[cycle_manager] cycle {} complete: {} bytes. Starting cycle {}",
        current_stats.cycle_number,
        current_stats.bytes_written,
        new_stats.cycle_number
    );

    Ok(new_stats)
}

pub fn format_bytes_human(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit_idx = 0;

    while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
        size /= 1024.0;
        unit_idx += 1;
    }

    format!("{:.2} {}", size, UNITS[unit_idx])
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// Re-export for convenience
use crate::feed_shared;
