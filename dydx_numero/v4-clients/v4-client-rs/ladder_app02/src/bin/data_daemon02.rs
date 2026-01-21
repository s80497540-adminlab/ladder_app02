// ladder_app02/src/bin/data_daemon02.rs
//
// Persistent dYdX mainnet data daemon. Runs headless 24/7, tails the
// websocket feed, and writes compact snapshots + append-only events to
// ./data so the UI can instantly hydrate when launched.

use anyhow::{anyhow, Context, Result};
use futures_util::{SinkExt, StreamExt};
use ladder_app02::feed_shared::{
    self, BookLevel, BookLevelsRecord, BookTopRecord, SnapshotState, TradeRecord,
};
use rustls::crypto::ring;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::time::sleep;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

const WS_MAINNET: &str = "wss://indexer.dydx.trade/v4/ws";
const DEFAULT_TICKERS: &[&str] = &["ETH-USD", "BTC-USD", "SOL-USD"];
const MAX_TICKERS_PER_CONN: usize = 20;
const MAX_TRADES: usize = 2000;
const SNAPSHOT_INTERVAL_SECS: u64 = 5;
const MARKET_HTTP: &str = "https://indexer.dydx.trade/v4/perpetualMarkets";

#[derive(Debug, serde::Serialize)]
#[serde(tag = "kind")]
enum PersistedEvent {
    #[serde(rename = "book_top")]
    BookTop { data: BookTopRecord },
    #[serde(rename = "book_levels")]
    BookLevels { data: BookLevelsRecord },
    #[serde(rename = "trade")]
    Trade { data: TradeRecord },
}

#[tokio::main]
async fn main() -> Result<()> {
    // Explicitly install the Ring crypto provider so rustls can build TLS configs.
    // This avoids the runtime panic that occurs when no default provider is set.
    ring::default_provider()
        .install_default()
        .map_err(|err| anyhow!("install rustls ring crypto provider: {err:?}"))?;

    println!("[data_daemon02] starting, writing to {:?}", feed_shared::data_dir());
    feed_shared::ensure_data_dir()?;

    let state = Arc::new(Mutex::new(SnapshotState::default()));
    let log_file = Arc::new(Mutex::new(open_log()?));

    let mut tickers = fetch_market_tickers().await;
    if tickers.is_empty() {
        tickers = DEFAULT_TICKERS.iter().map(|s| s.to_string()).collect();
    }
    prioritize_tickers(&mut tickers, DEFAULT_TICKERS);
    let chunks: Vec<Vec<String>> = tickers
        .chunks(MAX_TICKERS_PER_CONN)
        .map(|c| c.to_vec())
        .collect();
    println!(
        "[data_daemon02] subscribing to {} tickers across {} connections",
        tickers.len(),
        chunks.len()
    );

    for (idx, chunk) in chunks.into_iter().enumerate() {
        let state = Arc::clone(&state);
        let log_file = Arc::clone(&log_file);
        tokio::spawn(async move {
            loop {
                match run_connection(&chunk, idx, &state, &log_file).await {
                    Ok(_) => {
                        println!(
                            "[data_daemon02] stream {idx} ended cleanly, reconnecting"
                        );
                    }
                    Err(err) => {
                        eprintln!("[data_daemon02] stream {idx} error: {err:?}");
                    }
                }

                let state_guard = state.lock().await;
                if let Err(err) = persist_snapshot(&state_guard) {
                    eprintln!("[data_daemon02] snapshot persist error: {err:?}");
                }

                sleep(Duration::from_secs(3)).await;
            }
        });
    }

    loop {
        sleep(Duration::from_secs(3600)).await;
    }
}

async fn run_connection(
    tickers: &[String],
    idx: usize,
    state: &Arc<Mutex<SnapshotState>>,
    log_file: &Arc<Mutex<std::fs::File>>,
) -> Result<()> {
    let (mut ws, _) = connect_async(WS_MAINNET)
        .await
        .context("failed to connect to dYdX websocket")?;
    println!("[data_daemon02] connected to {WS_MAINNET} (stream {idx})");
    println!(
        "[data_daemon02] stream {idx} subscribing to {} tickers",
        tickers.len()
    );

    for tk in tickers {
        subscribe(&mut ws, "v4_orderbook", tk).await?;
        subscribe(&mut ws, "v4_trades", tk).await?;
    }

    let mut last_snapshot = Instant::now();

    while let Some(msg) = ws.next().await {
        match msg {
            Ok(Message::Text(txt)) => {
                let mut state_guard = state.lock().await;
                let mut log_guard = log_file.lock().await;
                if let Err(err) = handle_message(&txt, &mut state_guard, &mut log_guard) {
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
            let state_guard = state.lock().await;
            if let Err(err) = persist_snapshot(&state_guard) {
                eprintln!("[data_daemon02] snapshot persist error: {err:?}");
            }
            last_snapshot = Instant::now();
        }
    }

    Ok(())
}

fn prioritize_tickers(tickers: &mut Vec<String>, priority: &[&str]) {
    let mut seen = HashSet::new();
    tickers.retain(|t| seen.insert(t.clone()));

    let mut ordered = Vec::with_capacity(tickers.len() + priority.len());
    for &tk in priority {
        if let Some(pos) = tickers.iter().position(|t| t == tk) {
            tickers.remove(pos);
        }
        ordered.push(tk.to_string());
    }
    ordered.extend(tickers.iter().cloned());
    *tickers = ordered;
}

async fn fetch_market_tickers() -> Vec<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build();
    let Ok(client) = client else {
        return Vec::new();
    };
    let resp = client.get(MARKET_HTTP).send().await;
    let Ok(resp) = resp else {
        return Vec::new();
    };
    let json = resp.json::<Value>().await;
    let Ok(json) = json else {
        return Vec::new();
    };
    let Some(markets) = json.get("markets").and_then(|v| v.as_object()) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(markets.len());
    for (ticker, meta) in markets {
        if let Some(status) = meta.get("status").and_then(|v| v.as_str()) {
            if status != "ACTIVE" {
                continue;
            }
        }
        out.push(ticker.to_string());
    }
    out.sort();
    out
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

    let bid_levels = parse_levels(&bids);
    let ask_levels = parse_levels(&asks);
    let (best_bid, bid_liq, best_bid_raw) = levels_stats(&bid_levels, true);
    let (best_ask, ask_liq, best_ask_raw) = levels_stats(&ask_levels, false);

    if best_bid == 0.0 && best_ask == 0.0 {
        return Ok(());
    }

    let ts_unix = now_unix();
    let record = BookTopRecord {
        ts_unix,
        ticker: ticker.to_string(),
        best_bid,
        best_ask,
        best_bid_raw,
        best_ask_raw,
        bid_liq,
        ask_liq,
    };

    state.last_book = Some(record.clone());
    persist_event(log_file, &PersistedEvent::BookTop { data: record })?;

    let levels_record = BookLevelsRecord {
        ts_unix,
        ticker: ticker.to_string(),
        bids: bid_levels,
        asks: ask_levels,
    };
    persist_event(log_file, &PersistedEvent::BookLevels { data: levels_record })
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
        let price = tr.get("price").and_then(parse_num).unwrap_or(0.0);
        let price_raw = tr
            .get("price")
            .and_then(raw_string)
            .unwrap_or_else(|| price.to_string());

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
            price,
            price_raw,
            source: "dydx".to_string(),
        };

        state.recent_trades.push(rec.clone());
        state.trim_trades(MAX_TRADES);
        persist_event(log_file, &PersistedEvent::Trade { data: rec })?;
    }

    Ok(())
}

fn parse_levels(levels: &[Value]) -> Vec<BookLevel> {
    let mut out = Vec::with_capacity(levels.len());

    for level in levels.iter() {
        // Handle both array format ([price, size]) and object format ({"price": "...", "size": "..."}).
        let (price_opt, size_opt, price_raw, size_raw) = if let Some(arr) = level.as_array() {
            if arr.len() >= 2 {
                (
                    parse_num(&arr[0]),
                    parse_num(&arr[1]),
                    raw_string(&arr[0]),
                    raw_string(&arr[1]),
                )
            } else {
                (None, None, None, None)
            }
        } else if let Some(obj) = level.as_object() {
            (
                obj.get("price").and_then(parse_num),
                obj.get("size").and_then(parse_num),
                obj.get("price").and_then(raw_string),
                obj.get("size").and_then(raw_string),
            )
        } else {
            (None, None, None, None)
        };

        let price = price_opt.unwrap_or(0.0);
        let size = size_opt.unwrap_or(0.0);

        if price <= 0.0 || size <= 0.0 {
            continue;
        }

        let price_raw = price_raw.unwrap_or_else(|| price.to_string());
        let size_raw = size_raw.unwrap_or_else(|| size.to_string());
        out.push(BookLevel {
            price,
            size,
            price_raw,
            size_raw,
        });
    }

    out
}

fn levels_stats(levels: &[BookLevel], is_bid: bool) -> (f64, f64, String) {
    let mut best = 0.0;
    let mut total = 0.0;
    let mut best_raw = String::new();

    for level in levels {
        total += level.size;
        if best == 0.0 {
            best = level.price;
            best_raw = if level.price_raw.is_empty() {
                level.price.to_string()
            } else {
                level.price_raw.clone()
            };
        } else if is_bid {
            if level.price > best {
                best = level.price;
                best_raw = if level.price_raw.is_empty() {
                    level.price.to_string()
                } else {
                    level.price_raw.clone()
                };
            }
        } else {
            if level.price < best {
                best = level.price;
                best_raw = if level.price_raw.is_empty() {
                    level.price.to_string()
                } else {
                    level.price_raw.clone()
                };
            }
        }
    }

    (best, total, best_raw)
}

fn parse_num(v: &Value) -> Option<f64> {
    match v {
        Value::Number(num) => num.as_f64(),
        Value::String(s) => s.parse::<f64>().ok(),
        _ => None,
    }
}

fn raw_string(v: &Value) -> Option<String> {
    match v {
        Value::Number(num) => Some(num.to_string()),
        Value::String(s) => Some(s.to_string()),
        _ => None,
    }
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
