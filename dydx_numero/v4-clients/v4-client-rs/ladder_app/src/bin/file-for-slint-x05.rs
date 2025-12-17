// ladder_app02/src/main.rs
// Slint dYdX GUI with CSV data + Rhai bot + real trading via `dydx` crate

mod candle_agg;

slint::include_modules!();

use candle_agg::{Candle, CandleAgg};

use std::cell::RefCell;
use std::cmp::{max, min};
use std::collections::{BTreeMap, HashMap};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::str::FromStr;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bigdecimal::BigDecimal;
use chrono::{Local, TimeZone};
use rhai::{Engine, Scope};
use tokio::sync::mpsc;

use rustls::crypto::ring;

use dydx::config::ClientConfig;
use dydx::indexer::IndexerClient;
use dydx::node::{NodeClient, OrderBuilder, OrderSide, Wallet};

// ---------- small helpers ----------

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_secs()
}

// price keying like egui version
type PriceKey = i64;

fn price_to_key(price: f64) -> PriceKey {
    (price * 10_000.0).round() as PriceKey
}

fn key_to_price(k: PriceKey) -> f64 {
    k as f64 / 10_000.0
}

fn format_ts_local(ts: u64) -> String {
    let dt = Local
        .timestamp_opt(ts as i64, 0)
        .single()
        .unwrap_or_else(|| Local.timestamp_opt(0, 0).single().unwrap());
    dt.format("%Y-%m-%d %H:%M:%S").to_string()
}

// ---------- CSV event structures ----------

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
    candles: Vec<Candle>,
    trades: Vec<TradeCsvEvent>,
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

// ---------- CSV I/O ----------

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

fn append_trade_csv(base_dir: &Path, ticker: &str, source: &str, side: &str, size_str: &str) {
    let ts = now_unix();
    let _ = std::fs::create_dir_all(base_dir);
    let path = base_dir.join(format!("trades_{ticker}.csv"));

    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{ts},{ticker},{source},{side},{size_str}");
    }
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

// Simple TF (like 1m); can expose later
const TF_SECS: u64 = 60;

fn compute_snapshot_for(data: &TickerData, target_ts: u64) -> Snapshot {
    let mut bids: BTreeMap<PriceKey, f64> = BTreeMap::new();
    let mut asks: BTreeMap<PriceKey, f64> = BTreeMap::new();

    let mut agg = CandleAgg::new(TF_SECS);

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
        candles: series,
        trades,
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

// ---------- Trading machinery ----------

#[derive(Clone, Debug)]
enum TradeKind {
    Market,
    Limit,
}

#[derive(Clone, Debug)]
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

// ---------- Core app state (non-UI) ----------

struct AppCore {
    base_dir: PathBuf,
    ticker_data: HashMap<String, TickerData>,
    tickers: Vec<String>,
    current_ticker: String,

    mode: String,
    time_mode: String,

    last_reload_ts: u64,
    reload_secs: f64,

    engine: Engine,
    scope: Scope<'static>,
    script_text: String,
    script_last_error: Option<String>,

    bot_signal: String,
    bot_size: f64,
    bot_comment: String,
    bot_auto_trade: bool,
    bot_last_executed_signal: String,

    balance_usdc: f64,
    balance_pnl: f64,

    receipts: Vec<TradeReceipt>,

    trade_tx: mpsc::Sender<TradeCmd>,
}

impl AppCore {
    fn new(base_dir: PathBuf, tickers: Vec<String>, trade_tx: mpsc::Sender<TradeCmd>) -> Self {
        let current_ticker = tickers
            .get(0)
            .cloned()
            .unwrap_or_else(|| "ETH-USD".to_string());

        let mut engine = Engine::new();
        engine.set_max_expr_depths(64, 64);

        let mut scope = Scope::new();
        scope.set_value("bot_signal", "none".to_string());
        scope.set_value("bot_size", 0.0_f64);
        scope.set_value("bot_comment", "".to_string());

        let default_script = r#"
// Rhai bot script.
//
// Inputs set by Rust:
//   best_bid, best_ask, mid, spread, bid_liquidity_near, ask_liquidity_near
//
// Outputs:
//   bot_signal  = "none" | "buy" | "sell"
//   bot_size    = f64
//   bot_comment = string

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
"#.to_string();

        Self {
            base_dir,
            ticker_data: HashMap::new(),
            tickers,
            current_ticker,
            mode: "Live".to_string(),
            time_mode: "Local".to_string(),
            last_reload_ts: 0,
            reload_secs: 5.0,

            engine,
            scope,
            script_text: default_script,
            script_last_error: None,

            bot_signal: "none".to_string(),
            bot_size: 0.0,
            bot_comment: "".to_string(),
            bot_auto_trade: false,
            bot_last_executed_signal: "none".to_string(),

            balance_usdc: 1000.0,
            balance_pnl: 0.0,

            receipts: Vec::new(),

            trade_tx,
        }
    }

    fn load_all_tickers(&mut self) {
        println!(
            "AppCore::load_all_tickers: loading CSV data from {}",
            self.base_dir.display()
        );
        for tk in &self.tickers {
            if let Some(td) = load_ticker_data(&self.base_dir, tk) {
                println!(
                    "  {tk}: {} book, {} trades",
                    td.book_events.len(),
                    td.trade_events.len()
                );
                self.ticker_data.insert(tk.clone(), td);
            } else {
                println!("  {tk}: no CSV data found");
            }
        }
    }

    fn reload_current_ticker(&mut self) {
        let tk = self.current_ticker.clone();
        if let Some(td) = load_ticker_data(&self.base_dir, &tk) {
            println!(
                "[RELOAD] {}: {} book events, {} trades",
                tk,
                td.book_events.len(),
                td.trade_events.len()
            );
            self.ticker_data.insert(tk, td);
        } else {
            println!("[RELOAD] {}: no CSV data found after reload", tk);
        }
    }

    fn current_range(&self) -> Option<(u64, u64)> {
        self.ticker_data
            .get(&self.current_ticker)
            .map(|td| (td.min_ts, td.max_ts))
    }

    fn current_snapshot(&self) -> Option<Snapshot> {
        let td = self.ticker_data.get(&self.current_ticker)?;
        Some(compute_snapshot_for(td, td.max_ts))
    }

    fn push_receipt(&mut self, r: TradeReceipt) {
        self.receipts.push(r);
        if self.receipts.len() > 300 {
            self.receipts.remove(0);
        }
    }

    fn feed_scope_from_snapshot(&mut self, snap: &Snapshot) {
        let bm = compute_bubble_metrics(snap);

        self.scope.clear();
        self.scope
            .set_value("ticker", self.current_ticker.clone());
        self.scope.set_value("best_bid", bm.best_bid);
        self.scope.set_value("best_ask", bm.best_ask);
        self.scope.set_value("mid", bm.mid);
        self.scope.set_value("spread", bm.spread);
        self.scope
            .set_value("bid_liquidity_near", bm.bid_liq);
        self.scope
            .set_value("ask_liquidity_near", bm.ask_liq);

        self.scope
            .set_value("bot_signal", self.bot_signal.clone());
        self.scope.set_value("bot_size", self.bot_size);
        self.scope
            .set_value("bot_comment", self.bot_comment.clone());
    }

    fn read_bot_from_scope(&mut self) {
        if let Some(sig) = self.scope.get_value::<String>("bot_signal") {
            self.bot_signal = sig;
        } else {
            self.bot_signal = "none".to_string();
        }

        if let Some(size) = self.scope.get_value::<f64>("bot_size") {
            self.bot_size = size.max(0.0);
        } else {
            self.bot_size = 0.0;
        }

        if let Some(comment) = self.scope.get_value::<String>("bot_comment") {
            self.bot_comment = comment;
        } else {
            self.bot_comment.clear();
        }
    }

    fn run_script_once(&mut self, snap: &Snapshot) {
        self.script_last_error = None;

        self.feed_scope_from_snapshot(snap);
        let script_src = self.script_text.clone();

        match self.engine.eval_with_scope::<()>(&mut self.scope, &script_src) {
            Ok(()) => {
                self.read_bot_from_scope();
            }
            Err(e) => {
                self.script_last_error = Some(e.to_string());
                self.bot_signal = "none".to_string();
                self.bot_size = 0.0;
                self.bot_comment = "".to_string();
            }
        }
    }

    fn send_manual_order(&mut self, size_units: f64, leverage: f64, side_str: String) -> String {
        let side = match side_str.as_str() {
            "Buy" | "buy" => OrderSide::Buy,
            "Sell" | "sell" => OrderSide::Sell,
            _ => OrderSide::Buy,
        };

        let ticker = self.current_ticker.clone();
        let clean_size = size_units.max(0.0);
        let size_str = format!("{:.8}", clean_size);

        let size_bd = match BigDecimal::from_str(&size_str) {
            Ok(v) => v,
            Err(_) => {
                return "Invalid size".to_string();
            }
        };

        let cmd = TradeCmd {
            ticker: ticker.clone(),
            side: side.clone(),
            size: size_bd.clone(),
            kind: TradeKind::Market,
            limit_price: 0.0,
            leverage,
        };

        let _ = self.trade_tx.try_send(cmd);

        append_trade_csv(
            &self.base_dir,
            &ticker,
            "gui_manual",
            &format!("{:?}", side),
            &size_str,
        );

        let now = now_unix();
        let ticket_ticker = ticker.clone();
        let ticket_side = format!("{:?}", side);

        self.push_receipt(TradeReceipt {
            ts: now,
            ticker: ticket_ticker,
            side: ticket_side,
            kind: "ManualMarket".to_string(),
            size: size_str.clone(),
            status: "submitted".to_string(),
            comment: "manual".to_string(),
        });

        format!("Sent {:?} {} size {}", side, ticker, size_str)
    }
}

// ---------- async trader: real dYdX orders via dydx::node ----------

async fn run_trader(mut rx: mpsc::Receiver<TradeCmd>) {
    // Adjust path for testnet/mainnet config
    let config_path = "../client/tests/testnet.toml";

    let config = match ClientConfig::from_file(config_path).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[trader] failed to load {config_path}: {e}");
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
            .until(h.ahead(10));

        if limit_price > 0.0 {
            // simple guard; real limit can be wired later
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
                append_trade_csv(
                    Path::new("../data"),
                    &ticker,
                    "trader",
                    &format!("{:?}", side),
                    &size.to_string(),
                );
            }
            Err(e) => {
                eprintln!("[trader] place_order error: {e}");
            }
        }
    }
}

// ---------- rustls crypto provider ----------

fn init_crypto_provider() {
    let _ = ring::default_provider().install_default();
}

// ---------- main: wire AppCore <-> Slint ----------

fn main() {
    init_crypto_provider();

    // CSV dir shared with daemon
    // From ladder_app02/, ../data is the workspace-level data folder.
    let base_dir = PathBuf::from("../data");

    let tickers = vec![
        "ETH-USD".to_string(),
        "BTC-USD".to_string(),
        "SOL-USD".to_string(),
    ];

    // Tokio runtime in a background thread for trading
    let (trade_tx, trade_rx) = mpsc::channel::<TradeCmd>(64);
    thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        rt.block_on(run_trader(trade_rx));
    });

    // Shared core state
    let core = Rc::new(RefCell::new(AppCore::new(
        base_dir.clone(),
        tickers.clone(),
        trade_tx,
    )));
    core.borrow_mut().load_all_tickers();

    // Slint UI
    let app = AppWindow::new().expect("failed to create Slint AppWindow");

    // Initial UI wiring
    {
        let core_ref = core.borrow();
        app.set_mode(core_ref.mode.clone().into());
        app.set_time_mode(core_ref.time_mode.clone().into());
        app.set_current_ticker(core_ref.current_ticker.clone().into());
        app.set_balance_usdc(core_ref.balance_usdc as f32);
        app.set_balance_pnl(core_ref.balance_pnl as f32);
        app.set_script_text(core_ref.script_text.clone().into());

        if let Some((min_ts, max_ts)) = core_ref.current_range() {
            app.set_data_range(
                format!(
                    "Range: {} → {}",
                    format_ts_local(min_ts),
                    format_ts_local(max_ts)
                )
                .into(),
            );
        } else {
            app.set_data_range("No CSV data yet".into());
        }

        if let Some(snap) = core_ref.current_snapshot() {
            let bm = compute_bubble_metrics(&snap);
            app.set_mid_price(bm.mid as f32);
            app.set_best_bid(bm.best_bid as f32);
            app.set_best_ask(bm.best_ask as f32);
            app.set_spread(bm.spread as f32);
            app.set_imbalance(bm.imbalance as f32);

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
            app.set_bids(slint::ModelRc::new(
                slint::VecModel::from(bids_vec),
            ));

            let asks_vec: Vec<BookLevel> = snap
                .asks
                .iter()
                .take(20)
                .map(|(k, s)| BookLevel {
                    price: format!("{:.2}", key_to_price(*k)).into(),
                    size: format!("{:.4}", s).into(),
                })
                .collect();
            app.set_asks(slint::ModelRc::new(
                slint::VecModel::from(asks_vec),
            ));

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
        }
    }

    // --- Callbacks ---

    // MODE
    {
        let app_weak = app.as_weak();
        let core = Rc::clone(&core);
        app.on_mode_changed(move |new_mode| {
            let app = app_weak.unwrap();
            let mut core = core.borrow_mut();
            println!("[MODE] Changed to: {}", new_mode);
            core.mode = new_mode.to_string();
            app.set_mode(new_mode);
        });
    }

    // TICKER
    {
        let app_weak = app.as_weak();
        let core = Rc::clone(&core);
        app.on_ticker_changed(move |new_ticker| {
            let app = app_weak.unwrap();
            let mut core = core.borrow_mut();
            let new_tk = new_ticker.to_string();
            if !core.tickers.contains(&new_tk) {
                println!("[TICKER] Unknown ticker: {}", new_tk);
                return;
            }
            println!("[TICKER] Changed to: {}", new_tk);
            core.current_ticker = new_tk.clone();
            core.reload_current_ticker();

            app.set_current_ticker(new_tk.clone().into());

            if let Some((min_ts, max_ts)) = core.current_range() {
                app.set_data_range(
                    format!(
                        "Range: {} → {}",
                        format_ts_local(min_ts),
                        format_ts_local(max_ts)
                    )
                    .into(),
                );
            }

            if let Some(snap) = core.current_snapshot() {
                let bm = compute_bubble_metrics(&snap);
                app.set_mid_price(bm.mid as f32);
                app.set_best_bid(bm.best_bid as f32);
                app.set_best_ask(bm.best_ask as f32);
                app.set_spread(bm.spread as f32);
                app.set_imbalance(bm.imbalance as f32);

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
                app.set_bids(slint::ModelRc::new(
                    slint::VecModel::from(bids_vec),
                ));

                let asks_vec: Vec<BookLevel> = snap
                    .asks
                    .iter()
                    .take(20)
                    .map(|(k, s)| BookLevel {
                        price: format!("{:.2}", key_to_price(*k)).into(),
                        size: format!("{:.4}", s).into(),
                    })
                    .collect();
                app.set_asks(slint::ModelRc::new(
                    slint::VecModel::from(asks_vec),
                ));

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
            }
        });
    }

    // TIME MODE
    {
        let app_weak = app.as_weak();
        let core = Rc::clone(&core);
        app.on_time_mode_changed(move |new_time_mode| {
            let app = app_weak.unwrap();
            let mut core = core.borrow_mut();
            println!("[TIME MODE] Changed to: {}", new_time_mode);
            core.time_mode = new_time_mode.to_string();
            app.set_time_mode(new_time_mode);
        });
    }

    // RELOAD DATA
    {
        let app_weak = app.as_weak();
        let core = Rc::clone(&core);
        app.on_reload_data(move || {
            let app = app_weak.unwrap();
            let mut core = core.borrow_mut();
            println!(
                "[RELOAD button] Reloading data for {}",
                core.current_ticker
            );
            core.reload_current_ticker();

            if let Some((min_ts, max_ts)) = core.current_range() {
                app.set_data_range(
                    format!(
                        "Range: {} → {}",
                        format_ts_local(min_ts),
                        format_ts_local(max_ts)
                    )
                    .into(),
                );
            }

            if let Some(snap) = core.current_snapshot() {
                let bm = compute_bubble_metrics(&snap);
                app.set_mid_price(bm.mid as f32);
                app.set_best_bid(bm.best_bid as f32);
                app.set_best_ask(bm.best_ask as f32);
                app.set_spread(bm.spread as f32);
                app.set_imbalance(bm.imbalance as f32);

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
                app.set_bids(slint::ModelRc::new(
                    slint::VecModel::from(bids_vec),
                ));

                let asks_vec: Vec<BookLevel> = snap
                    .asks
                    .iter()
                    .take(20)
                    .map(|(k, s)| BookLevel {
                        price: format!("{:.2}", key_to_price(*k)).into(),
                        size: format!("{:.4}", s).into(),
                    })
                    .collect();
                app.set_asks(slint::ModelRc::new(
                    slint::VecModel::from(asks_vec),
                ));

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
            }
        });
    }

    // SCRIPT RUN
    {
        let app_weak = app.as_weak();
        let core = Rc::clone(&core);
        app.on_run_script(move || {
            let app = app_weak.unwrap();
            let mut core = core.borrow_mut();

            if let Some(snap) = core.current_snapshot() {
                let script = app.get_script_text().to_string();
                core.script_text = script.clone();
                core.run_script_once(&snap);

                app.set_bot_signal(core.bot_signal.clone().into());
                app.set_bot_size(core.bot_size as f32);
                app.set_bot_comment(core.bot_comment.clone().into());

                if let Some(err) = &core.script_last_error {
                    app.set_script_error(err.clone().into());
                } else {
                    app.set_script_error("OK".into());
                }
            } else {
                app.set_script_error("No snapshot yet".into());
            }
        });
    }

    // SEND ORDER
    {
        let app_weak = app.as_weak();
        let core = Rc::clone(&core);
        app.on_send_order(move || {
            let app = app_weak.unwrap();
            let mut core = core.borrow_mut();
            let side = app.get_trade_side().to_string();
            let size = app.get_trade_size() as f64;
            let lev = app.get_trade_leverage() as f64;

            let msg = core.send_manual_order(size, lev, side.clone());
            app.set_order_message(msg.clone().into());

            // update receipts UI
            let receipts_vec: Vec<Receipt> = core
                .receipts
                .iter()
                .rev()
                .take(200)
                .map(|r| Receipt {
                    ts: format_ts_local(r.ts).into(),
                    ticker: r.ticker.clone().into(),
                    side: r.side.clone().into(),
                    kind: r.kind.clone().into(),
                    size: r.size.clone().into(),
                    status: r.status.clone().into(),
                    comment: r.comment.clone().into(),
                })
                .collect();
            app.set_receipts(slint::ModelRc::new(
                slint::VecModel::from(receipts_vec),
            ));
        });
    }

    // TIMER: current time label
    {
        let app_weak = app.as_weak();
        let timer = slint::Timer::default();
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_secs(1),
            move || {
                let app = app_weak.unwrap();
                let now_s = format_ts_local(now_unix());
                app.set_current_time(now_s.into());
            },
        );
    }

    println!("Starting Slint Trading GUI...");
    println!("Using CSV dir: {}", base_dir.display());

    app.run().unwrap();
}
