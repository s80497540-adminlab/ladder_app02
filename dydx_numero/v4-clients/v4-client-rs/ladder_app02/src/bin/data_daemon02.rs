// ladder_app02/src/bin/data_daemon02.rs
//
// Persistent dYdX mainnet data daemon. Runs headless 24/7, tails the
// websocket feed, and writes compact snapshots + append-only events to
// ./data so the UI can instantly hydrate when launched.

use anyhow::{anyhow, Context, Result};
use futures_util::{SinkExt, StreamExt};
use ladder_app02::feed_shared::{self, BookTopRecord, SnapshotState, TradeRecord};
use serde_json::{json, Value};
use std::fs::{create_dir_all, OpenOptions};
use std::io::Write;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::net::TcpStream;
use tokio::time::sleep;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

const WS_MAINNET: &str = "wss://api.dydx.exchange/v4/ws";
const TICKERS: &[&str] = &["ETH-USD", "BTC-USD", "SOL-USD"];
const MAX_TRADES: usize = 2000;
const SNAPSHOT_INTERVAL_SECS: u64 = 5;

#[derive(Debug, serde::Serialize)]
#[serde(tag = "kind")]
enum PersistedEvent {
    #[serde(rename = "book_top")]
    BookTop { data: BookTopRecord },
    #[serde(rename = "trade")]
    Trade { data: TradeRecord },
}

#[tokio::main]
async fn main() -> Result<()> {
    println!("[data_daemon02] starting, writing to ./data");
    install_rustls_provider()?;

    create_dir_all(feed_shared::DATA_DIR)?;

    let mut state = SnapshotState::default();
    let mut log_file = open_log()?;

    loop {
        match run_connection(&mut state, &mut log_file).await {
            Ok(_) => {
                println!("[data_daemon02] stream ended cleanly, reconnecting");
            }
            Err(err) => {
                eprintln!("[data_daemon02] error: {err:?}");
            }
        }

        // Persist the current snapshot before reconnecting so the UI can still read something.
        if let Err(err) = persist_snapshot(&state) {
            eprintln!("[data_daemon02] snapshot persist error: {err:?}");
        }

        sleep(Duration::from_secs(3)).await;
    }
}

async fn run_connection(state: &mut SnapshotState, log_file: &mut std::fs::File) -> Result<()> {
    let (mut ws, _) = connect_async(WS_MAINNET)
        .await
        .context("failed to connect to dYdX websocket")?;
    println!("[data_daemon02] connected to {WS_MAINNET}");

    for tk in TICKERS {
        subscribe(&mut ws, "v4_orderbook", tk).await?;
        subscribe(&mut ws, "v4_trades", tk).await?;
    }

    let mut last_snapshot = Instant::now();

    while let Some(msg) = ws.next().await {
        match msg {
            Ok(Message::Text(txt)) => {
                if let Err(err) = handle_message(&txt, state, log_file) {
                    eprintln!("[data_daemon02] handle_message error: {err:?}");
                }
            }
            Ok(Message::Binary(_)) => {
                // ignore
            }
            Ok(Message::Ping(payload)) => {
                ws.send(Message::Pong(payload)).await.ok();
            }
            Ok(Message::Close(frame)) => {
                println!("[data_daemon02] close frame: {:?}", frame);
                break;
            }
            Err(err) => {
                return Err(err.into());
            }
            _ => {}
        }

        if last_snapshot.elapsed().as_secs() >= SNAPSHOT_INTERVAL_SECS {
            if let Err(err) = persist_snapshot(state) {
                eprintln!("[data_daemon02] snapshot persist error: {err:?}");
            }
            last_snapshot = Instant::now();
        }
    }

    Ok(())
}

async fn subscribe(
    ws: &mut WebSocketStream<MaybeTlsStream<TcpStream>>,
    channel: &str,
    id: &str,
) -> Result<()> {
    let msg = json!({
        "type": "subscribe",
        "channel": channel,
        "id": id,
    });
    ws.send(Message::Text(msg.to_string()))
        .await
        .with_context(|| format!("failed to subscribe to {channel} {id}"))
}

fn handle_message(
    txt: &str,
    state: &mut SnapshotState,
    log_file: &mut std::fs::File,
) -> Result<()> {
    let v: Value = serde_json::from_str(txt).context("invalid websocket json")?;
    let msg_type = v.get("type").and_then(Value::as_str).unwrap_or_default();

    if msg_type != "channel_data" {
        return Ok(());
    }

    let channel = v
        .get("channel")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing channel"))?;
    let id = v.get("id").and_then(Value::as_str).unwrap_or("UNKNOWN");
    let contents = v
        .get("contents")
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()));

    match channel {
        "v4_orderbook" => handle_orderbook(id, &contents, state, log_file),
        "v4_trades" => handle_trades(id, &contents, state, log_file),
        _ => Ok(()),
    }
}

fn handle_orderbook(
    ticker: &str,
    contents: &Value,
    state: &mut SnapshotState,
    log_file: &mut std::fs::File,
) -> Result<()> {
    let bids = contents
        .get("bids")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let asks = contents
        .get("asks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let (best_bid, bid_liq) = parse_side(&bids);
    let (best_ask, ask_liq) = parse_side(&asks);

    if best_bid == 0.0 && best_ask == 0.0 {
        return Ok(());
    }

    let record = BookTopRecord {
        ts_unix: now_unix(),
        ticker: ticker.to_string(),
        best_bid,
        best_ask,
        bid_liq,
        ask_liq,
    };

    state.last_book = Some(record.clone());
    persist_event(log_file, &PersistedEvent::BookTop { data: record })
}

fn handle_trades(
    ticker: &str,
    contents: &Value,
    state: &mut SnapshotState,
    log_file: &mut std::fs::File,
) -> Result<()> {
    let trades = contents
        .get("trades")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    for tr in trades {
        let side = tr
            .get("side")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let size = tr
            .get("size")
            .and_then(Value::as_str)
            .unwrap_or("0")
            .to_string();

        let ts_unix = tr
            .get("createdAt")
            .and_then(Value::as_str)
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.timestamp() as u64)
            .unwrap_or_else(now_unix);

        let rec = TradeRecord {
            ts_unix,
            ticker: ticker.to_string(),
            side: if side.eq_ignore_ascii_case("buy") {
                "Buy"
            } else {
                "Sell"
            }
            .to_string(),
            size: size.clone(),
            source: "dydx".to_string(),
        };

        state.recent_trades.push(rec.clone());
        state.trim_trades(MAX_TRADES);
        persist_event(log_file, &PersistedEvent::Trade { data: rec })?;
    }

    Ok(())
}

fn parse_side(levels: &[Value]) -> (f64, f64) {
    let mut best = 0.0;
    let mut total = 0.0;

    for level in levels.iter() {
        if let Some(arr) = level.as_array() {
            if arr.len() >= 2 {
                let price = arr[0]
                    .as_str()
                    .and_then(|s| s.parse::<f64>().ok())
                    .unwrap_or(0.0);
                let size = arr[1]
                    .as_str()
                    .and_then(|s| s.parse::<f64>().ok())
                    .unwrap_or(0.0);

                if best == 0.0 {
                    best = price;
                }
                total += size;
            }
        }
    }

    (best, total)
}

fn persist_event(log_file: &mut std::fs::File, evt: &PersistedEvent) -> Result<()> {
    let line = serde_json::to_string(evt)?;
    log_file.write_all(line.as_bytes())?;
    log_file.write_all(b"\n")?;
    Ok(())
}

fn persist_snapshot(state: &SnapshotState) -> Result<()> {
    let path = feed_shared::snapshot_path();
    let json = serde_json::to_string_pretty(state)?;
    std::fs::write(path, json)?;
    Ok(())
}

fn open_log() -> Result<std::fs::File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(feed_shared::event_log_path())
        .context("unable to open log file")
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_secs()
}

fn install_rustls_provider() -> Result<()> {
    // Rustls 0.23 requires a process-wide crypto provider. Opt into the ring
    // backend explicitly so the websocket handshake can succeed. If another
    // part of the process already installed a provider, keep running.
    rustls::crypto::ring::default_provider()
        .install_default()
        .or_else(|_| Ok(()))
        .map_err(|err| anyhow!("failed to install rustls ring provider: {err:?}"))
}
