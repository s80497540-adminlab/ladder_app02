use crate::app::{AppEvent, FeedEvent};
use crate::debug_hooks;
use ladder_app02::feed_shared::{self, BookTopRecord, SnapshotState, TradeRecord};
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

const MAX_BOOTSTRAP_TRADES: usize = 500;

fn read_snapshot(path: &PathBuf) -> Option<SnapshotState> {
    match std::fs::read_to_string(path) {
        Ok(raw) => match serde_json::from_str::<SnapshotState>(&raw) {
            Ok(snap) => {
                debug_hooks::log_snapshot_result(
                    "loaded",
                    format!(
                        "last_book={} trades={}",
                        snap.last_book.is_some(),
                        snap.recent_trades.len()
                    ),
                );
                Some(snap)
            }
            Err(err) => {
                debug_hooks::log_snapshot_result("parse_error", format!("{err}"));
                None
            }
        },
        Err(err) => {
            debug_hooks::log_snapshot_result("read_error", format!("{}", err));
            None
        }
    }
}

fn send_book(top: &BookTopRecord, tx: &std::sync::mpsc::Sender<AppEvent>) {
    let _ = tx.send(AppEvent::Feed(FeedEvent::BookTop {
        ts_unix: top.ts_unix,
        ticker: top.ticker.clone(),
        best_bid: top.best_bid,
        best_ask: top.best_ask,
        bid_liq: top.bid_liq,
        ask_liq: top.ask_liq,
    }));
}

fn send_trade(trade: &TradeRecord, tx: &std::sync::mpsc::Sender<AppEvent>) {
    let _ = tx.send(AppEvent::Feed(FeedEvent::Trade {
        ts_unix: trade.ts_unix,
        ticker: trade.ticker.clone(),
        side: trade.side.clone(),
        size: trade.size.clone(),
        source: trade.source.clone(),
    }));
}

#[derive(Debug, serde::Deserialize)]
#[serde(tag = "kind")]
enum PersistedLine {
    #[serde(rename = "book_top")]
    BookTop { data: BookTopRecord },
    #[serde(rename = "trade")]
    Trade { data: TradeRecord },
}

pub fn start_daemon_bridge(tx: std::sync::mpsc::Sender<AppEvent>) {
    thread::spawn(move || {
        let snapshot_path = feed_shared::snapshot_path();
        let log_path = feed_shared::event_log_path();

        debug_hooks::log_feed_bridge_start(&snapshot_path, &log_path);

        // Bootstrap from snapshot
        if let Some(mut snap) = read_snapshot(&snapshot_path) {
            snap.trim_trades(MAX_BOOTSTRAP_TRADES);
            if let Some(book) = snap.last_book.as_ref() {
                debug_hooks::log_snapshot_result(
                    "apply_book",
                    format!(
                        "ts={} ticker={} bid={} ask={} bid_liq={} ask_liq={}",
                        book.ts_unix,
                        book.ticker,
                        book.best_bid,
                        book.best_ask,
                        book.bid_liq,
                        book.ask_liq
                    ),
                );
                send_book(book, &tx);
            }
            for trade in snap.recent_trades.iter() {
                debug_hooks::log_snapshot_result(
                    "apply_trade",
                    format!(
                        "ts={} ticker={} side={} size={}",
                        trade.ts_unix, trade.ticker, trade.side, trade.size
                    ),
                );
                send_trade(trade, &tx);
            }
        }

        let mut offset: u64 = 0;
        let mut idle_loops: u64 = 0;
        loop {
            if let Ok(file) = OpenOptions::new().read(true).open(&log_path) {
                let mut reader = BufReader::new(file);
                if let Ok(file_len) = reader.get_ref().metadata().map(|m| m.len()) {
                    if file_len < offset {
                        debug_hooks::log_event_log_issue("file shrunk; resetting offset to 0");
                        offset = 0; // file rotated or truncated
                    }
                }
                if reader.seek(SeekFrom::Start(offset)).is_ok() {
                    let mut line = String::new();
                    while let Ok(bytes) = reader.read_line(&mut line) {
                        if bytes == 0 {
                            idle_loops += 1;
                            break;
                        }
                        idle_loops = 0;
                        if let Ok(parsed) = serde_json::from_str::<PersistedLine>(&line) {
                            match parsed {
                                PersistedLine::BookTop { data } => send_book(&data, &tx),
                                PersistedLine::Trade { data } => send_trade(&data, &tx),
                            }
                        } else if let Err(err) = serde_json::from_str::<PersistedLine>(&line) {
                            debug_hooks::log_event_parse_error(&line, &err.to_string());
                        }
                        line.clear();
                    }
                    if let Ok(pos) = reader.seek(SeekFrom::Current(0)) {
                        offset = pos;
                    }
                }
            } else {
                debug_hooks::log_event_log_issue(format!("unable to open {:?}", log_path));
            }

            debug_hooks::log_bridge_idle(idle_loops, offset);
            thread::sleep(Duration::from_millis(500));
        }
    });
}
