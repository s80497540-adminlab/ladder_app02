use chrono::Utc;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

static ENABLED: OnceLock<bool> = OnceLock::new();
static FILE_HANDLE: OnceLock<Mutex<std::fs::File>> = OnceLock::new();

fn logging_enabled() -> bool {
    *ENABLED.get_or_init(|| {
        std::env::var("LADDER_DEBUG_HOOKS")
            .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
            .unwrap_or(false)
    })
}

fn log_file() -> &'static Mutex<std::fs::File> {
    FILE_HANDLE.get_or_init(|| {
        let _ = std::fs::create_dir_all("data");
        let path = Path::new("data").join("debug_hooks.log");
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap_or_else(|_| {
                std::fs::File::create("/tmp/debug_hooks.log").expect("fallback log create")
            });
        Mutex::new(file)
    })
}

fn log_line(topic: &str, msg: impl AsRef<str>) {
    if !logging_enabled() {
        return;
    }

    let ts = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let formatted = format!("[{ts}][{topic}] {}", msg.as_ref());

    if let Ok(mut f) = log_file().lock() {
        let _ = writeln!(f, "{formatted}");
    }

    eprintln!("{formatted}");
}

pub fn log_feed_bridge_start(snapshot_path: &Path, log_path: &Path) {
    log_line(
        "feed.bridge",
        format!(
            "starting bridge; snapshot={:?} log={:?} env=LADDER_DEBUG_HOOKS={}",
            snapshot_path,
            log_path,
            std::env::var("LADDER_DEBUG_HOOKS").unwrap_or_else(|_| "(unset)".into())
        ),
    );
}

pub fn log_snapshot_result(result: &str, detail: impl AsRef<str>) {
    log_line("feed.snapshot", format!("{}: {}", result, detail.as_ref()));
}

pub fn log_event_log_issue(detail: impl AsRef<str>) {
    log_line("feed.event_log", detail);
}

pub fn log_event_parse_error(line: &str, err: &str) {
    log_line(
        "feed.parse",
        format!("failed to parse line: {line:?}; err={err}"),
    );
}

pub fn log_bridge_idle(loop_count: u64, offset: u64) {
    static LAST_LOGGED: OnceLock<Mutex<u64>> = OnceLock::new();
    if !logging_enabled() {
        return;
    }
    let guard = LAST_LOGGED.get_or_init(|| Mutex::new(0));
    if let Ok(mut last) = guard.lock() {
        if loop_count == 0 || loop_count - *last >= 10 {
            *last = loop_count;
            log_line(
                "feed.bridge",
                format!("no new lines after {loop_count} checks; offset={offset}"),
            );
        }
    }
}

pub fn log_book_ingest(
    ts_unix: u64,
    ticker: &str,
    best_bid: f64,
    best_ask: f64,
    bid_liq: f64,
    ask_liq: f64,
) {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let n = COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    if n <= 10 || n % 50 == 0 {
        log_line(
            "feed.book",
            format!(
                "book tick #{n} ts={ts_unix} ticker={ticker} bid={best_bid} ask={best_ask} bid_liq={bid_liq} ask_liq={ask_liq}"
            ),
        );
    }
}

pub fn log_book_skip(reason: &str, detail: impl AsRef<str>) {
    log_line(
        "feed.book.skip",
        format!("{} | {}", reason, detail.as_ref()),
    );
}

pub fn log_placeholder_ladder(
    best_bid: f64,
    best_ask: f64,
    depth: usize,
    bid_liq: f64,
    ask_liq: f64,
) {
    log_line(
        "feed.ladder",
        format!(
            "building placeholder ladder depth={} best_bid={} best_ask={} bid_liq={} ask_liq={}",
            depth, best_bid, best_ask, bid_liq, ask_liq
        ),
    );
}

pub fn log_trade_ingest(ts_unix: u64, ticker: &str, side: &str, size: &str) {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let n = COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    if n <= 20 || n % 100 == 0 {
        log_line(
            "feed.trade",
            format!("trade #{n} ts={ts_unix} ticker={ticker} side={side} size={size}"),
        );
    }
}

pub fn log_trade_skip(reason: &str, detail: impl AsRef<str>) {
    log_line(
        "feed.trade.skip",
        format!("{} | {}", reason, detail.as_ref()),
    );
}

pub fn log_candle_reset(reason: &str) {
    log_line("candle.reset", reason);
}

pub fn log_mid_bucket(ts_unix: u64, bucket: u64, mid: f64, candles: usize) {
    log_line(
        "candle.mid",
        format!("mid tick ts={ts_unix} bucket={bucket} mid={mid} candle_count={candles}"),
    );
}

pub fn log_candle_gap(prev: u64, new_bucket: u64) {
    log_line(
        "candle.gap",
        format!("gap detected; prev_bucket={prev} new_bucket={new_bucket}"),
    );
}

pub fn log_candle_volume(ts_unix: u64, size: f64, candle_ts: Option<String>) {
    log_line(
        "candle.volume",
        format!(
            "added volume size={size} ts={ts_unix} candle={:?}",
            candle_ts
        ),
    );
}
