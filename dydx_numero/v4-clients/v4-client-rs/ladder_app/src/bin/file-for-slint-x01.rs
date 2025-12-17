// src/main.rs
// Slint version of the dYdX trading GUI with CSV + Rhai + bot + async trader.

mod candle_agg;

slint::include_modules!();

use crate::candle_agg::{CandleAgg, Candle};

use std::cell::RefCell;
use std::cmp::{max, min};
use std::collections::{BTreeMap, HashMap};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bigdecimal::BigDecimal;
use chrono::{Local, TimeZone};
use rhai::{Engine, Scope};
use tokio::sync::mpsc;

// dYdX client pieces – adjust crate names if needed.
use dydx_client::config::ClientConfig;
use dydx_client::indexer::IndexerClient;
use dydx_client::node::{NodeClient, OrderBuilder, OrderSide, Wallet};
use dydx_proto::dydxprotocol::clob::order::TimeInForce;

// ---------- basic helpers ----------

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_secs()
}

fn format_ts_local(ts: u64) -> String {
    let dt = Local
        .timestamp_opt(ts as i64, 0)
        .single()
        .unwrap_or_else(|| Local.timestamp_opt(0, 0).single().unwrap());
    dt.format("%Y-%m-%d %H:%M:%S").to_string()
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum TimeMode {
    Local,
    Unix,
}

impl TimeMode {
    fn from_str(s: &str) -> Self {
        match s {
            "Unix" => TimeMode::Unix,
            _ => TimeMode::Local,
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            TimeMode::Local => "Local",
            TimeMode::Unix => "Unix",
        }
    }
}

fn format_ts(mode: TimeMode, ts: u64) -> String {
    match mode {
        TimeMode::Unix => format!("{ts}"),
        TimeMode::Local => format_ts_local(ts),
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Mode {
    Live,
    Replay,
}

impl Mode {
    fn from_str(s: &str) -> Self {
        match s {
            "Replay" => Mode::Replay,
            _ => Mode::Live,
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            Mode::Live => "Live",
            Mode::Replay => "Replay",
        }
    }
}

// ---------- price / book helpers ----------

type PriceKey = i64;

fn price_to_key(price: f64) -> PriceKey {
    (price * 10_000.0).round() as PriceKey
}

fn key_to_price(key: PriceKey) -> f64 {
    key as f64 / 10_000.0
}

// ---------- CSV + data structures ----------

#[derive(Clone, Debug)]
struct BookCsvEvent {
    ts: u64,
    ticker: String,
    side: String,
    price: f64,
    size: f64,
}

#[derive(Clone, Debug)]
struct TradeCsvEvent {
    ts: u64,
    ticker: String,
    side: String,
    size_str: String,
}

#[derive(Clone, Debug, Default)]
struct TickerData {
    ticker: String,
    book_events: Vec<BookCsvEvent>,
    trade_events: Vec<TradeCsvEvent>,
    min_ts: u64,
    max_ts: u64,
}

#[derive(Clone, Debug, Default)]
struct Snapshot {
    bids: BTreeMap<PriceKey, f64>,
    asks: BTreeMap<PriceKey, f64>,
    trades: Vec<TradeCsvEvent>,
    candles: Vec<Candle>,
    last_mid: f64,
    last_vol: f64,
}

#[derive(Clone, Debug, Default)]
struct BubbleMetrics {
    best_bid: f64,
    best_ask: f64,
    mid: f64,
    spread: f64,
    bid_liq: f64,
    ask_liq: f64,
    imbalance: f64,
}

fn load_book_csv(path: &Path, ticker: &str) -> Vec<BookCsvEvent> {
    if !path.exists() {
        return Vec::new();
    }
    let Ok(f) = File::open(path) else {
        return Vec::new();
    };
    let reader = BufReader::new(f);
    let mut out = Vec::new();

    for line in reader.lines().flatten() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split(',').collect();
        if parts.len() < 6 {
            continue;
        }
        let Ok(ts) = parts[0].parse::<u64>() else {
            continue;
        };
        let tk = parts[1].trim_matches('"').to_string();
        if tk != ticker {
            continue;
        }
        let _kind = parts[2].to_string(); // currently unused
        let side = parts[3].to_string();
        let Ok(price) = parts[4].parse::<f64>() else {
            continue;
        };
        let Ok(size) = parts[5].parse::<f64>() else {
            continue;
        };
        out.push(BookCsvEvent {
            ts,
            ticker: tk,
            side,
            price,
            size,
        });
    }

    out.sort_by_key(|e| e.ts);
    out
}

fn load_trades_csv(path: &Path, ticker: &str) -> Vec<TradeCsvEvent> {
    if !path.exists() {
        return Vec::new();
    }
    let Ok(f) = File::open(path) else {
        return Vec::new();
    };
    let reader = BufReader::new(f);
    let mut out = Vec::new();

    for line in reader.lines().flatten() {
        let line = line.trim();
        if line.is_empty() {
            continue;
;
        }
        let parts: Vec<&str> = line.split(',').collect();
        if parts.len() < 5 {
            continue;
        }
        let Ok(ts) = parts[0].parse::<u64>() else {
            continue;
        };
        let tk = parts[1].trim_matches('"').to_string();
        if tk != ticker {
            continue;
        }
        let _source = parts[2].to_string();
        let side = parts[3].to_string();
        let size_str = parts[4].to_string();

        out.push(TradeCsvEvent {
            ts,
            ticker: tk,
            side,
            size_str,
        });
    }

    out.sort_by_key(|t| t.ts);
    out
}

fn load_ticker_data(base_dir: &Path, ticker: &str) -> Option<TickerData> {
    let ob_path = base_dir.join(format!("orderbook_{ticker}.csv"));
    let tr_path = base_dir.join(format!("trades_{ticker}.csv"));

    let book_events = load_book_csv(&ob_path, ticker);
    let trade_events = load_trades_csv(&tr_path, ticker);

    if book_events.is_empty() && trade_events.is_empty() {
        return None;
    }

    let mut min_ts = u64::MAX;
    let mut max_ts = 0u64;

    for e in &book_events {
        min_ts = min(min_ts, e.ts);
        max_ts = max(max_ts, e.ts);
    }
    for e in &trade_events {
        min_ts = min(min_ts, e.ts);
        max_ts = max(max_ts, e.ts);
    }

    if min_ts == u64::MAX {
        return None;
    }

    Some(TickerData {
        ticker: ticker.to_string(),
        book_events,
        trade_events,
        min_ts,
        max_ts,
    })
}

fn compute_snapshot_for(data: &TickerData, target_ts: u64, tf_secs: u64) -> Snapshot {
    let mut bids: BTreeMap<PriceKey, f64> = BTreeMap::new();
    let mut asks: BTreeMap<PriceKey, f64> = BTreeMap::new();
    let mut agg = CandleAgg::new(tf_secs);

    for e in &data.book_events {
        if e.ts > target_ts {
            break;
        }

        let map = if e.side.to_lowercase() == "bid" {
            &mut bids
        } else {
            &mut asks
        };

        let key = price_to_key(e.price);

        if e.size == 0.0 {
            map.remove(&key);
        } else {
            map.insert(key, e.size);
        }

        if let (Some((bp, _)), Some((ap, _))) = (bids.iter().next_back(), asks.iter().next()) {
            let mid = (key_to_price(*bp) + key_to_price(*ap)) * 0.5;
            let vol = e.size.abs().max(0.0);
            agg.update(e.ts, mid, vol);
        }
    }

    let mut trades: Vec<TradeCsvEvent> = data
        .trade_events
        .iter()
        .filter(|t| t.ts <= target_ts)
        .cloned()
        .collect();
    trades.sort_by_key(|t| t.ts);
    if trades.len() > 200 {
        let start = trades.len() - 200;
        trades = trades[start..].to_vec();
    }

    let series = agg.series().to_vec();
    let (last_mid, last_vol) = if let Some(c) = series.last() {
        (c.close, c.volume)
    } else {
        (0.0, 0.0)
    };

    Snapshot {
        bids,
        asks,
        trades,
        candles: series,
        last_mid,
        last_vol,
    }
}

fn compute_bubble_metrics(snap: &Snapshot) -> BubbleMetrics {
    let best_bid = snap
        .bids
        .iter()
        .next_back()
        .map(|(k, _)| key_to_price(*k))
        .unwrap_or(0.0);
    let best_ask = snap
        .asks
        .iter()
        .next()
        .map(|(k, _)| key_to_price(*k))
        .unwrap_or(0.0);
    let mid = if best_bid > 0.0 && best_ask > 0.0 {
        (best_bid + best_ask) * 0.5
    } else {
        0.0
    };
    let spread = if best_bid > 0.0 && best_ask > 0.0 {
        best_ask - best_bid
    } else {
        0.0
    };

    let mut bid_liq = 0.0;
    for (_, s) in snap.bids.iter().rev().take(10) {
        bid_liq += *s;
    }
    let mut ask_liq = 0.0;
    for (_, s) in snap.asks.iter().take(10) {
        ask_liq += *s;
    }

    let imbalance = if ask_liq > 0.0 {
        bid_liq / ask_liq
    } else {
        0.0
    };

    BubbleMetrics {
        best_bid,
        best_ask,
        mid,
        spread,
        bid_liq,
        ask_liq,
        imbalance,
    }
}

// ---------- trading + receipts ----------

#[derive(Clone, Debug)]
enum TradeKind {
    Market,
    Limit,
}

#[derive(Debug)]
struct TradeCmd {
    ticker: String,
    side: OrderSide,
    size: BigDecimal,
    kind: TradeKind,
    limit_price: f64,
    leverage: f64,
}

#[derive(Clone, Debug)]
struct TradeReceipt {
    ts: u64,
    ticker: String,
    side: String,
    kind: String,
    size: String,
    status: String,
    comment: String,
}

fn append_trade_csv(ticker: &str, source: &str, side: &str, size_str: &str) {
    let ts = now_unix();
    let dir = Path::new("data");
    let _ = std::fs::create_dir_all(dir);
    let path = dir.join(format!("trades_{ticker}.csv"));

    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{ts},{ticker},{source},{side},{size_str}");
    }
}

fn init_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

// async trade executor – same idea as egui version
async fn run_trader(mut rx: mpsc::Receiver<TradeCmd>) {
    let config = match ClientConfig::from_file("client/tests/testnet.toml").await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[trader] failed to load testnet.toml: {e}");
            return;
        }
    };

    let raw = match std::env::var("DYDX_TESTNET_MNEMONIC") {
        Ok(v) => v,
        Err(_) => {
            eprintln!("[trader] DYDX_TESTNET_MNEMONIC not set; trading disabled");
            return;
        }
    };
    let mnemonic = raw.split_whitespace().collect::<Vec<_>>().join(" ");

    let wallet = match Wallet::from_mnemonic(&mnemonic) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("[trader] invalid mnemonic: {e}");
            return;
        }
    };

    let mut node = match NodeClient::connect(config.node).await {
        Ok(n) => n,
        Err(e) => {
            eprintln!("[trader] node connect failed: {e}");
            return;
        }
    };

    let mut account = match wallet.account(0, &mut node).await {
        Ok(a) => a,
        Err(e) => {
            eprintln!("[trader] account sync failed: {e}");
            return;
        }
    };

    let sub = match account.subaccount(0) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[trader] subaccount derive failed: {e}");
            return;
        }
    };

    let indexer = IndexerClient::new(config.indexer);

    while let Some(cmd) = rx.recv().await {
        let TradeCmd {
            ticker,
            side,
            size,
            kind,
            limit_price,
            leverage: _,
        } = cmd;

        eprintln!(
            "[trader] {:?} {:?} {} size {} (limit guard: {})",
            kind, side, ticker, size, limit_price
        );

        let market = match indexer
            .markets()
            .get_perpetual_market(&ticker.clone().into())
            .await
        {
            Ok(m) => m,
            Err(e) => {
                eprintln!("[trader] market meta error for {ticker}: {e}");
                continue;
            }
        };

        let h = match node.latest_block_height().await {
            Ok(h) => h,
            Err(e) => {
                eprintln!("[trader] height error: {e}");
                continue;
            }
        };

        let mut builder = OrderBuilder::new(market, sub.clone())
            .market(side.clone(), size.clone())
            .reduce_only(false)
            .time_in_force(TimeInForce::Unspecified)
            .until(h.ahead(10));

        if limit_price > 0.0 {
            // placeholder guard – adjust to real Price type as needed
            builder = builder.price(100);
        }

        let (_id, order) = match builder.build(123456) {
            Ok(x) => x,
            Err(e) => {
                eprintln!("[trader] build order error: {e}");
                continue;
            }
        };

        match node.place_order(&mut account, order).await {
            Ok(tx_hash) => {
                eprintln!(
                    "[trader] placed {:?} {} size {} tx={tx_hash:?}",
                    side, ticker, size
                );
                append_trade_csv(&ticker, "trader", &format!("{:?}", side), &size.to_string());
            }
            Err(e) => {
                eprintln!("[trader] place_order error: {e}");
            }
        }
    }
}

// ---------- AppCore: shared state between Slint + logic ----------

struct AppCore {
    base_dir: PathBuf,
    ticker_data: HashMap<String, TickerData>,
    tickers: Vec<String>,
    current_ticker: String,

    mode: Mode,
    time_mode: TimeMode,

    live_ts: u64,
    replay_ts: u64,

    reload_secs: u64,
    last_reload_ts: u64,

    tf_secs: u64,
    history_candles: usize,

    engine: Engine,
    scope: Scope<'static>,
    script_last_error: Option<String>,
    script_last_run_ts: u64,

    bot_last_executed_signal: String,

    trade_tx: mpsc::Sender<TradeCmd>,

    balance_usdc: f64,
    balance_pnl: f64,

    receipts: Vec<TradeReceipt>,
}

impl AppCore {
    fn new(base_dir: PathBuf, tickers: Vec<String>, trade_tx: mpsc::Sender<TradeCmd>) -> Self {
        let current_ticker = tickers
            .get(0)
            .cloned()
            .unwrap_or_else(|| "ETH-USD".to_string());

        let mut ticker_data = HashMap::new();
        for tk in &tickers {
            if let Some(td) = load_ticker_data(&base_dir, tk) {
                ticker_data.insert(tk.clone(), td);
            }
        }

        let (live_ts, replay_ts) = ticker_data
            .get(&current_ticker)
            .map(|td| (td.max_ts, td.max_ts))
            .unwrap_or((now_unix(), now_unix()));

        let mut engine = Engine::new();
        engine.set_max_expr_depths(64, 64);
        let scope = Scope::new();

        Self {
            base_dir,
            ticker_data,
            tickers,
            current_ticker,
            mode: Mode::Live,
            time_mode: TimeMode::Local,
            live_ts,
            replay_ts,
            reload_secs: 5,
            last_reload_ts: now_unix(),
            tf_secs: 60,
            history_candles: 200,
            engine,
            scope,
            script_last_error: None,
            script_last_run_ts: 0,
            bot_last_executed_signal: "none".to_string(),
            trade_tx,
            balance_usdc: 1000.0,
            balance_pnl: 0.0,
            receipts: Vec::new(),
        }
    }

    fn ticker_range(&self) -> Option<(u64, u64)> {
        self.ticker_data
            .get(&self.current_ticker)
            .map(|td| (td.min_ts, td.max_ts))
    }

    fn reload_current_ticker(&mut self) {
        if let Some(td) = load_ticker_data(&self.base_dir, &self.current_ticker) {
            self.live_ts = td.max_ts;
            if self.replay_ts < td.min_ts || self.replay_ts > td.max_ts {
                self.replay_ts = td.max_ts;
            }
            self.ticker_data.insert(self.current_ticker.clone(), td);
        }
    }

    fn push_receipt(&mut self, r: TradeReceipt) {
        self.receipts.push(r);
        if self.receipts.len() > 300 {
            self.receipts.remove(0);
        }
    }
}

// ---------- script + bot ----------

fn run_bot_script(core: &mut AppCore, app: &AppWindow, snap: &Snapshot) {
    core.script_last_error = None;

    let bm = compute_bubble_metrics(snap);

    core.scope.clear();
    core.scope
        .set_value("ticker", core.current_ticker.clone());
    core.scope
        .set_value("mode", match core.mode {
            Mode::Live => "live".to_string(),
            Mode::Replay => "replay".to_string(),
        });
    core.scope.set_value("best_bid", bm.best_bid);
    core.scope.set_value("best_ask", bm.best_ask);
    core.scope.set_value("mid", bm.mid);
    core.scope.set_value("spread", bm.spread);
    core.scope
        .set_value("bid_liquidity_near", bm.bid_liq);
    core.scope
        .set_value("ask_liquidity_near", bm.ask_liq);
    core.scope
        .set_value("tf_secs", core.tf_secs as i64);
    core.scope
        .set_value("history_candles", core.history_candles as i64);

    // seed with current bot state from UI
    let current_signal: String = app.get_bot_signal().into();
    let current_size = app.get_bot_size() as f64;
    let current_comment: String = app.get_bot_comment().into();
    core.scope
        .set_value("bot_signal", current_signal);
    core.scope.set_value("bot_size", current_size);
    core.scope
        .set_value("bot_comment", current_comment);

    let script_text: String = app.get_script_text().into();

    let res = core
        .engine
        .eval_with_scope::<()>(&mut core.scope, &script_text);

    match res {
        Ok(()) => {
            let new_signal = core
                .scope
                .get_value::<String>("bot_signal")
                .unwrap_or_else(|| "none".to_string());
            let new_size = core
                .scope
                .get_value::<f64>("bot_size")
                .unwrap_or(0.0)
                .max(0.0);
            let new_comment = core
                .scope
                .get_value::<String>("bot_comment")
                .unwrap_or_default();

            app.set_bot_signal(new_signal.clone().into());
            app.set_bot_size(new_size as f32);
            app.set_bot_comment(new_comment.clone().into());
            app.set_script_error("".into());

            // auto trade
            if app.get_bot_auto_trade()
                && (new_signal == "buy" || new_signal == "sell")
                && new_signal != core.bot_last_executed_signal
                && new_size > 0.0
            {
                let maybe_side = match new_signal.as_str() {
                    "buy" => Some(OrderSide::Buy),
                    "sell" => Some(OrderSide::Sell),
                    _ => None,
                };
                if let Some(side) = maybe_side {
                    let size_str = format!("{:.8}", new_size.max(0.0));
                    if let Ok(size_bd) = BigDecimal::from_str(&size_str) {
                        let leverage = app.get_trade_leverage() as f64;
                        let cmd = TradeCmd {
                            ticker: core.current_ticker.clone(),
                            side: side.clone(),
                            size: size_bd.clone(),
                            kind: TradeKind::Market,
                            limit_price: 0.0,
                            leverage,
                        };
                        let _ = core.trade_tx.try_send(cmd);
                        app.set_order_message(
                            format!(
                                "[BOT] auto {:?} {} size {}",
                                side, core.current_ticker, size_str
                            )
                            .into(),
                        );
                        append_trade_csv(
                            &core.current_ticker,
                            "bot_auto",
                            &format!("{:?}", side),
                            &size_str,
                        );
                        core.push_receipt(TradeReceipt {
                            ts: now_unix(),
                            ticker: core.current_ticker.clone(),
                            side: format!("{:?}", side),
                            kind: "BotAuto".to_string(),
                            size: size_str,
                            status: "submitted".to_string(),
                            comment: new_comment,
                        });
                        core.bot_last_executed_signal = new_signal;
                    }
                }
            }
        }
        Err(e) => {
            let msg = e.to_string();
            app.set_script_error(msg.clone().into());
            core.script_last_error = Some(msg);
        }
    }

    core.script_last_run_ts = now_unix();
}

// ---------- UI sync ----------

fn update_ui_from_state(app: &AppWindow, core: &mut AppCore) {
    // ensure ticker exists
    let Some(td) = core.ticker_data.get(&core.current_ticker) else {
        app.set_data_range("No data".into());
        return;
    };

    let (min_ts, max_ts) = (td.min_ts, td.max_ts);

    // adjust replay/live ts
    match core.mode {
        Mode::Live => {
            core.live_ts = max_ts;
            core.replay_ts = max_ts;
        }
        Mode::Replay => {
            if core.replay_ts < min_ts {
                core.replay_ts = min_ts;
            }
            if core.replay_ts > max_ts {
                core.replay_ts = max_ts;
            }
        }
    }

    let target_ts = match core.mode {
        Mode::Live => max_ts,
        Mode::Replay => core.replay_ts,
    };

    let snap = compute_snapshot_for(td, target_ts, core.tf_secs);
    let bm = compute_bubble_metrics(&snap);

    // metrics into UI
    app.set_mid_price(bm.mid as f32);
    app.set_best_bid(bm.best_bid as f32);
    app.set_best_ask(bm.best_ask as f32);
    app.set_spread(bm.spread as f32);
    app.set_imbalance(bm.imbalance as f32);

    // balances
    app.set_balance_usdc(core.balance_usdc as f32);
    app.set_balance_pnl(core.balance_pnl as f32);

    // range + current time
    let range_str = format!(
        "Range: {} → {}",
        format_ts(core.time_mode, min_ts),
        format_ts(core.time_mode, max_ts)
    );
    app.set_data_range(range_str.into());

    let cur_ts_str = format_ts(core.time_mode, target_ts);
    app.set_current_time(cur_ts_str.into());

    // ladder: bids/asks
    let bids_vec: Vec<BookLevel> = snap
        .bids
        .iter()
        .rev()
        .take(20)
        .map(|(k, s)| BookLevel {
            price: format!("{:.2}", key_to_price(*k)).into(),
            size: format!("{:.4}", s).into(),
        })
        .collect();
    let asks_vec: Vec<BookLevel> = snap
        .asks
        .iter()
        .take(20)
        .map(|(k, s)| BookLevel {
            price: format!("{:.2}", key_to_price(*k)).into(),
            size: format!("{:.4}", s).into(),
        })
        .collect();
    app.set_bids(slint::ModelRc::new(slint::VecModel::from(
        bids_vec,
    )));
    app.set_asks(slint::ModelRc::new(slint::VecModel::from(
        asks_vec,
    )));

    // recent trades
    let trades_vec: Vec<Trade> = snap
        .trades
        .iter()
        .rev()
        .take(50)
        .map(|t| Trade {
            ts: t.ts as i32,
            side: t.side.clone().into(),
            size: t.size_str.clone().into(),
        })
        .collect();
    app.set_recent_trades(slint::ModelRc::new(
        slint::VecModel::from(trades_vec),
    ));

    // receipts
    let rec_vec: Vec<Receipt> = core
        .receipts
        .iter()
        .rev()
        .take(200)
        .map(|r| Receipt {
            ts: format_ts(core.time_mode, r.ts).into(),
            ticker: r.ticker.clone().into(),
            side: r.side.clone().into(),
            kind: r.kind.clone().into(),
            size: r.size.clone().into(),
            status: r.status.clone().into(),
            comment: r.comment.clone().into(),
        })
        .collect();
    app.set_receipts(slint::ModelRc::new(
        slint::VecModel::from(rec_vec),
    ));

    // script auto-run on each refresh from latest snapshot
    run_bot_script(core, app, &snap);
}

// ---------- main ----------

fn main() {
    init_crypto_provider();

    // adjust base_dir as needed
    let base_dir = PathBuf::from("data");
    let tickers = vec![
        "ETH-USD".to_string(),
        "BTC-USD".to_string(),
        "SOL-USD".to_string(),
    ];

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let (trade_tx, trade_rx) = mpsc::channel::<TradeCmd>(64);
    rt.spawn(run_trader(trade_rx));

    let core = Rc::new(RefCell::new(AppCore::new(
        base_dir.clone(),
        tickers.clone(),
        trade_tx.clone(),
    )));

    let app = AppWindow::new().unwrap();

    // default script (similar to egui version)
    let default_script = r#"
// Rhai bot script.
// Inputs:
//   ticker, mode, best_bid, best_ask, mid, spread,
//   bid_liquidity_near, ask_liquidity_near, tf_secs, history_candles
// Outputs:
//   bot_signal, bot_size, bot_comment

let imbalance = if ask_liquidity_near > 0.0 {
    bid_liquidity_near / ask_liquidity_near
} else {
    0.0
};

bot_signal = "none";
bot_size = 0.0;
bot_comment = "";

if imbalance > 2.5 && spread < mid * 0.0005 {
    bot_signal = "buy";
    bot_size = 0.01;
    bot_comment = "Bid bubble detected";
} else if imbalance < 0.4 && spread < mid * 0.0005 {
    bot_signal = "sell";
    bot_size = 0.01;
    bot_comment = "Ask bubble detected";
}
"#;
    app.set_script_text(default_script.into());

    // initial UI setup
    {
        let mut core_mut = core.borrow_mut();
        app.set_current_ticker(core_mut.current_ticker.clone().into());
        app.set_mode(core_mut.mode.as_str().into());
        app.set_time_mode(core_mut.time_mode.as_str().into());
        app.set_balance_usdc(core_mut.balance_usdc as f32);
        app.set_balance_pnl(core_mut.balance_pnl as f32);
        update_ui_from_state(&app, &mut core_mut);
    }

    // --- callbacks ---

    // send_order
    {
        let core = core.clone();
        let app_weak = app.as_weak();
        app.on_send_order(move || {
            let app = app_weak.unwrap();
            let mut core = core.borrow_mut();

            let side_str: String = app.get_trade_side().into();
            let side_lower = side_str.to_lowercase();
            let side = if side_lower == "buy" {
                OrderSide::Buy
            } else {
                OrderSide::Sell
            };

            let size_val = app.get_trade_size() as f64;
            let size_val = size_val.max(0.0);
            let size_str = format!("{:.8}", size_val);

            if let Ok(size_bd) = BigDecimal::from_str(&size_str) {
                let leverage = app.get_trade_leverage() as f64;
                let cmd = TradeCmd {
                    ticker: core.current_ticker.clone(),
                    side: side.clone(),
                    size: size_bd.clone(),
                    kind: TradeKind::Market,
                    limit_price: 0.0,
                    leverage,
                };
                let _ = core.trade_tx.try_send(cmd);
                app.set_order_message(
                    format!(
                        "Sent {:?} {} size {}",
                        side, core.current_ticker, size_str
                    )
                    .into(),
                );
                append_trade_csv(
                    &core.current_ticker,
                    "gui_manual",
                    &format!("{:?}", side),
                    &size_str,
                );
                core.push_receipt(TradeReceipt {
                    ts: now_unix(),
                    ticker: core.current_ticker.clone(),
                    side: format!("{:?}", side),
                    kind: "Manual".to_string(),
                    size: size_str,
                    status: "submitted".to_string(),
                    comment: "manual".to_string(),
                });
                update_ui_from_state(&app, &mut core);
            } else {
                app.set_order_message("Invalid size for order".into());
            }
        });
    }

    // reload_data
    {
        let core = core.clone();
        let app_weak = app.as_weak();
        app.on_reload_data(move || {
            let app = app_weak.unwrap();
            let mut core = core.borrow_mut();
            println!("[RELOAD] Reloading data for {}", core.current_ticker);
            core.reload_current_ticker();
            core.last_reload_ts = now_unix();
            app.set_order_message("Data reloaded".into());
            update_ui_from_state(&app, &mut core);
        });
    }

    // run_script (manual)
    {
        let core = core.clone();
        let app_weak = app.as_weak();
        app.on_run_script(move || {
            let app = app_weak.unwrap();
            let mut core = core.borrow_mut();
            if let Some(td) = core.ticker_data.get(&core.current_ticker) {
                let target_ts = match core.mode {
                    Mode::Live => td.max_ts,
                    Mode::Replay => core.replay_ts,
                };
                let snap = compute_snapshot_for(td, target_ts, core.tf_secs);
                run_bot_script(&mut core, &app, &snap);
            }
        });
    }

    // ticker_changed
    {
        let core = core.clone();
        let app_weak = app.as_weak();
        app.on_ticker_changed(move |new_ticker| {
            let app = app_weak.unwrap();
            let mut core = core.borrow_mut();
            let t: String = new_ticker.into();
            println!("[TICKER] Changed to: {}", t);
            core.current_ticker = t.clone();
            core.reload_current_ticker();
            app.set_current_ticker(t.into());
            update_ui_from_state(&app, &mut core);
        });
    }

    // mode_changed
    {
        let core = core.clone();
        let app_weak = app.as_weak();
        app.on_mode_changed(move |new_mode| {
            let app = app_weak.unwrap();
            let mut core = core.borrow_mut();
            let m: String = new_mode.into();
            println!("[MODE] Changed to: {}", m);
            core.mode = Mode::from_str(&m);
            app.set_mode(core.mode.as_str().into());
            update_ui_from_state(&app, &mut core);
        });
    }

    // time_mode_changed
    {
        let core = core.clone();
        let app_weak = app.as_weak();
        app.on_time_mode_changed(move |new_time_mode| {
            let app = app_weak.unwrap();
            let mut core = core.borrow_mut();
            let m: String = new_time_mode.into();
            println!("[TIME MODE] Changed to: {}", m);
            core.time_mode = TimeMode::from_str(&m);
            app.set_time_mode(core.time_mode.as_str().into());
            update_ui_from_state(&app, &mut core);
        });
    }

    // deposit / withdraw (sim balances) – these callbacks are defined in Slint
    {
        let core = core.clone();
        let app_weak = app.as_weak();
        app.on_deposit(move |amt| {
            let app = app_weak.unwrap();
            let mut core = core.borrow_mut();
            let val = (amt as f64).abs();
            core.balance_usdc += val;
            core.push_receipt(TradeReceipt {
                ts: now_unix(),
                ticker: core.current_ticker.clone(),
                side: "N/A".to_string(),
                kind: "DepositSim".to_string(),
                size: format!("{:.2}", val),
                status: "ok".to_string(),
                comment: "Sim deposit".to_string(),
            });
            update_ui_from_state(&app, &mut core);
        });
    }

    {
        let core = core.clone();
        let app_weak = app.as_weak();
        app.on_withdraw(move |amt| {
            let app = app_weak.unwrap();
            let mut core = core.borrow_mut();
            let val = (amt as f64).abs();
            if core.balance_usdc >= val {
                core.balance_usdc -= val;
                core.push_receipt(TradeReceipt {
                    ts: now_unix(),
                    ticker: core.current_ticker.clone(),
                    side: "N/A".to_string(),
                    kind: "WithdrawSim".to_string(),
                    size: format!("{:.2}", val),
                    status: "ok".to_string(),
                    comment: "Sim withdraw".to_string(),
                });
            } else {
                core.push_receipt(TradeReceipt {
                    ts: now_unix(),
                    ticker: core.current_ticker.clone(),
                    side: "N/A".to_string(),
                    kind: "WithdrawSim".to_string(),
                    size: format!("{:.2}", val),
                    status: "fail".to_string(),
                    comment: "Insufficient sim balance".to_string(),
                });
            }
            update_ui_from_state(&app, &mut core);
        });
    }

    // periodic timer: reload + replay step + UI refresh
    {
        let core = core.clone();
        let app_weak = app.as_weak();
        let timer = slint::Timer::default();
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(1000),
            move || {
                let app = app_weak.unwrap();
                let mut core = core.borrow_mut();
                let now = now_unix();

                // periodic CSV reload
                if now.saturating_sub(core.last_reload_ts) >= core.reload_secs {
                    core.reload_current_ticker();
                    core.last_reload_ts = now;
                }

                // simple replay stepping
                if let Some((min_ts, max_ts)) = core.ticker_range() {
                    match core.mode {
                        Mode::Live => {
                            core.live_ts = max_ts;
                            core.replay_ts = max_ts;
                        }
                        Mode::Replay => {
                            if core.replay_ts < min_ts || core.replay_ts > max_ts {
                                core.replay_ts = min_ts;
                            } else if core.replay_ts < max_ts {
                                core.replay_ts += 1;
                            }
                        }
                    }
                }

                update_ui_from_state(&app, &mut core);
            },
        );
    }

    println!("Starting Slint Trading GUI...");
    println!("Loading data from: {}", base_dir.display());

    app.run().unwrap();
}
