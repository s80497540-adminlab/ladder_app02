use crate::app::{AppEvent, FeedEvent};
use ladder_app02::feed_shared::{self, BookTopRecord, SnapshotState, TradeRecord};
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

const MAX_BOOTSTRAP_TRADES: usize = 500;

fn read_snapshot(path: &PathBuf) -> Option<SnapshotState> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<SnapshotState>(&s).ok())
}

fn send_book(top: &BookTopRecord, tx: &std::sync::mpsc::Sender<AppEvent>) {
    let _ = tx.send(AppEvent::Feed(FeedEvent::BookTop {
        ts_unix: top.ts_unix,
        best_bid: top.best_bid,
        best_ask: top.best_ask,
        bid_liq: top.bid_liq,
        ask_liq: top.ask_liq,
    }));
}

fn send_trade(trade: &TradeRecord, tx: &std::sync::mpsc::Sender<AppEvent>) {
    let _ = tx.send(AppEvent::Feed(FeedEvent::Trade {
        ts_unix: trade.ts_unix,
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

        // Bootstrap from snapshot
        if let Some(mut snap) = read_snapshot(&snapshot_path) {
            snap.trim_trades(MAX_BOOTSTRAP_TRADES);
            if let Some(book) = snap.last_book.as_ref() {
                send_book(book, &tx);
            }
            for trade in snap.recent_trades.iter() {
                send_trade(trade, &tx);
            }
        }

        let mut offset: u64 = 0;
        loop {
            if let Ok(file) = OpenOptions::new().read(true).open(&log_path) {
                let mut reader = BufReader::new(file);
                if let Ok(file_len) = reader.get_ref().metadata().map(|m| m.len()) {
                    if file_len < offset {
                        offset = 0; // file rotated or truncated
                    }
                }
                if reader.seek(SeekFrom::Start(offset)).is_ok() {
                    let mut line = String::new();
                    while let Ok(bytes) = reader.read_line(&mut line) {
                        if bytes == 0 {
                            break;
                        }
                        if let Ok(parsed) = serde_json::from_str::<PersistedLine>(&line) {
                            match parsed {
                                PersistedLine::BookTop { data } => send_book(&data, &tx),
                                PersistedLine::Trade { data } => send_trade(&data, &tx),
                            }
                        }
                        line.clear();
                    }
                    if let Ok(pos) = reader.seek(SeekFrom::Current(0)) {
                        offset = pos;
                    }
                }
            }

            thread::sleep(Duration::from_millis(500));
        }
    });
}
