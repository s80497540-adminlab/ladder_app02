// ladder_app/src/bin/full_gui_x08.rs
//
// GUI that uses ONLY daemon-written CSVs under ./data:
//   - orderbook_{TICKER}.csv : ts,ticker,kind,side,price,size
//   - trades_{TICKER}.csv    : ts,ticker,source,side,size_str
//
// Live mode:
//   - Periodically reloads CSVs
//   - "Live" = snapshot at latest ts for current ticker
//
// Replay mode:
//   - Slider over [min_ts..max_ts] from CSVs
//
// Shared:
//   - Ticker dropdown: ETH-USD / BTC-USD / SOL-USD
//   - Time display: Unix vs Local
//   - Y-axis: auto/manual, shift+scroll vertical zoom
//   - Candles + volume
//   - Depth ladder + top-of-book tables + recent trades
//   - Trading panel (market orders) -> TradeCmd -> run_trader
//   - Rhai script engine for appearance + bot signal
//
// Script engine:
//   Exposed variables before script runs:
//
//     let ctx = #{              // Rhai map
//       mid:          f64,      // last mid
//       last_vol:     f64,      // last candle volume
//       tf:           i64,      // current timeframe seconds
//       history:      i64,      // candles shown
//       auto_y:       bool,
//       y_min:        f64,
//       y_max:        f64,
//       show_depth:   bool,
//       show_ladders: bool,
//       show_trades:  bool,
//       show_volume:  bool,
//       reload_secs:  f64,
//       mode:         "live" | "replay",
//       ticker:       String,
//     };
//
//   You can override:
//
//     tf            : i64    (seconds, >0)
//     history       : i64    (10..=5000)
//     auto_y        : bool
//     y_min, y_max  : f64
//     show_depth    : bool
//     show_ladders  : bool
//     show_trades   : bool
//     show_volume   : bool
//     reload_secs   : f64 (1.0..=600.0)
//     bot_signal    : String ("BUY" / "SELL" / "FLAT" / whatever)
//     bot_size      : f64
//     bot_comment   : String
//
//   Example script:
//
//     let mid = ctx["mid"];
//     if mid > 0.0 {
//         tf = 60;
//         history = 400;
//         auto_y = true;
//         show_depth = true;
//         show_ladders = true;
//         show_trades = true;
//         show_volume = true;
//
//         if ctx["mode"] == "live" && mid > 4000.0 {
//             bot_signal = "BUY";
//             bot_size   = 0.01;
//             bot_comment = "Mid > 4000";
//         } else {
//             bot_signal = "FLAT";
//             bot_size   = 0.0;
//         }
//     }
//
// Run:
//
//   export DYDX_TESTNET_MNEMONIC='...'
//   cargo run --release -p ladder_app --bin full_gui_x08
//

mod candle_agg;

use candle_agg::{Candle, CandleAgg};

use chrono::{Local, TimeZone};
use eframe::egui;
use egui::{Color32, RichText};
use egui_plot::{Line, Plot, PlotBounds, PlotPoints, VLine};
use rhai::{Engine, EvalAltResult, Map as RhaiMap, Scope};

use bigdecimal::BigDecimal;
use dydx_client::config::ClientConfig;
use dydx_client::indexer::IndexerClient;
use dydx_client::node::{NodeClient, OrderBuilder, OrderSide, Wallet};
use dydx_proto::dydxprotocol::clob::order::TimeInForce;

use std::cmp::{max, min};
use std::collections::{BTreeMap, HashMap};
use std::env;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::str::FromStr;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::{mpsc, watch};

// ---------- helpers ----------

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_secs()
}

type PriceKey = i64;

fn price_to_key(price: f64) -> PriceKey {
    (price * 10_000.0).round() as PriceKey
}

fn key_to_price(key: PriceKey) -> f64 {
    key as f64 / 10_000.0
}

// ---------- time display ----------

#[derive(Clone, Copy, PartialEq, Eq)]
enum TimeDisplayMode {
    Unix,
    Local,
}

impl TimeDisplayMode {
    fn label(self) -> &'static str {
        match self {
            TimeDisplayMode::Unix => "Unix",
            TimeDisplayMode::Local => "Local",
        }
    }
}

fn format_ts(mode: TimeDisplayMode, ts: u64) -> String {
    match mode {
        TimeDisplayMode::Unix => format!("{ts}"),
        TimeDisplayMode::Local => {
            let dt = Local
                .timestamp_opt(ts as i64, 0)
                .single()
                .unwrap_or_else(|| Local.timestamp_opt(0, 0).single().unwrap());
            dt.format("%Y-%m-%d %H:%M:%S").to_string()
        }
    }
}

// ---------- modes & settings ----------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Live,
    Replay,
}

#[derive(Clone)]
struct ChartSettings {
    show_candles: usize,
    auto_y: bool,
    y_min: f64,
    y_max: f64,
    selected_tf: u64,
}

impl Default for ChartSettings {
    fn default() -> Self {
        Self {
            show_candles: 300,
            auto_y: true,
            y_min: 0.0,
            y_max: 0.0,
            selected_tf: 60,
        }
    }
}

#[derive(Clone)]
struct LayoutSettings {
    show_depth: bool,
    show_ladders: bool,
    show_trades: bool,
    show_volume: bool,
}

impl Default for LayoutSettings {
    fn default() -> Self {
        Self {
            show_depth: true,
            show_ladders: true,
            show_trades: true,
            show_volume: true,
        }
    }
}

// ---------- CSV structures ----------

#[derive(Clone, Debug)]
struct BookCsvEvent {
    ts: u64,
    ticker: String,
    kind: String,
    side: String,
    price: f64,
    size: f64,
}

#[derive(Clone, Debug)]
struct TradeCsvEvent {
    ts: u64,
    ticker: String,
    source: String,
    side: String,
    size_str: String,
}

#[derive(Clone, Debug)]
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
    last_mid: f64,
    last_vol: f64,
    trades: Vec<TradeCsvEvent>,
}

// ---------- CSV loading ----------

fn load_book_csv(base_dir: &str, ticker: &str) -> Vec<BookCsvEvent> {
    let path = Path::new(base_dir).join(format!("orderbook_{ticker}.csv"));
    if !path.exists() {
        return Vec::new();
    }

    let f = match File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let reader = BufReader::new(f);
    let mut out = Vec::new();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let parts: Vec<&str> = trimmed.split(',').collect();
        if parts.len() < 6 {
            continue;
        }

        let ts = match parts[0].parse::<u64>() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let tk = parts[1].trim_matches('"').to_string();
        let kind = parts[2].to_string();
        let side = parts[3].to_string();
        let price = match parts[4].parse::<f64>() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let size = match parts[5].parse::<f64>() {
            Ok(v) => v,
            Err(_) => continue,
        };

        if tk != ticker {
            continue;
        }

        out.push(BookCsvEvent {
            ts,
            ticker: tk,
            kind,
            side,
            price,
            size,
        });
    }

    out.sort_by_key(|e| e.ts);
    out
}

fn load_trades_csv(base_dir: &str, ticker: &str) -> Vec<TradeCsvEvent> {
    let path = Path::new(base_dir).join(format!("trades_{ticker}.csv"));
    if !path.exists() {
        return Vec::new();
    }

    let f = match File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let reader = BufReader::new(f);
    let mut out = Vec::new();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let parts: Vec<&str> = trimmed.split(',').collect();
        if parts.len() < 5 {
            continue;
        }

        let ts = match parts[0].parse::<u64>() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let tk = parts[1].trim_matches('"').to_string();
        let source = parts[2].to_string();
        let side = parts[3].to_string();
        let size_str = parts[4].to_string();

        if tk != ticker {
            continue;
        }

        out.push(TradeCsvEvent {
            ts,
            ticker: tk,
            source,
            side,
            size_str,
        });
    }

    out.sort_by_key(|e| e.ts);
    out
}

fn load_ticker_data(base_dir: &str, ticker: &str) -> Option<TickerData> {
    let book_events = load_book_csv(base_dir, ticker);
    let trade_events = load_trades_csv(base_dir, ticker);

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

// compute snapshot at target_ts for a given timeframe
fn compute_snapshot_for(td: &TickerData, target_ts: u64, tf_secs: u64) -> Snapshot {
    let mut bids: BTreeMap<PriceKey, f64> = BTreeMap::new();
    let mut asks: BTreeMap<PriceKey, f64> = BTreeMap::new();

    let mut agg = CandleAgg::new(tf_secs);

    for e in td.book_events.iter().filter(|e| e.ts <= target_ts) {
        let book = if e.side.to_lowercase() == "bid" {
            &mut bids
        } else {
            &mut asks
        };

        let key = price_to_key(e.price);
        if e.size == 0.0 {
            book.remove(&key);
        } else {
            book.insert(key, e.size);
        }

        if let (Some((bp, _)), Some((ap, _))) = (bids.iter().next_back(), asks.iter().next()) {
            let mid = (key_to_price(*bp) + key_to_price(*ap)) * 0.5;
            let vol = e.size.abs().max(0.0);
            agg.update(e.ts, mid, vol);
        }
    }

    let candles = agg.series().to_vec();
    let (last_mid, last_vol) = if let Some(c) = candles.last() {
        (c.close, c.volume)
    } else {
        (0.0, 0.0)
    };

    let mut trades: Vec<TradeCsvEvent> = td
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

    Snapshot {
        bids,
        asks,
        candles,
        last_mid,
        last_vol,
        trades,
    }
}

// ---------- crypto init ----------

fn init_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

// ---------- trading ----------

#[derive(Debug)]
enum TradeCmd {
    Market {
        ticker: String,
        side: OrderSide,
        size: BigDecimal,
        leverage: f64,
    },
}

async fn run_trader(mut rx: mpsc::Receiver<TradeCmd>) {
    let config = match ClientConfig::from_file("client/tests/testnet.toml").await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[trader] Failed to load testnet.toml: {e}");
            return;
        }
    };

    let raw = match env::var("DYDX_TESTNET_MNEMONIC") {
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

    let mut indexer = IndexerClient::new(config.indexer);

    while let Some(cmd) = rx.recv().await {
        match cmd {
            TradeCmd::Market {
                ticker,
                side,
                size,
                leverage,
            } => {
                eprintln!(
                    "[trader] market {:?} {} size {} lev {}",
                    side, ticker, size, leverage
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

                let (_id, order) = match OrderBuilder::new(market, sub.clone())
                    .market(side, size.clone())
                    .reduce_only(false)
                    .price(100) // placeholder slippage guard
                    .time_in_force(TimeInForce::Unspecified)
                    .until(h.ahead(10))
                    .build(123456)
                {
                    Ok(x) => x,
                    Err(e) => {
                        eprintln!("[trader] build order error: {e}");
                        continue;
                    }
                };

                match node.place_order(&mut account, order).await {
                    Ok(tx_hash) => {
                        eprintln!(
                            "[trader] placed {:?} {} size {} lev {} tx={tx_hash:?}",
                            side, ticker, size, leverage
                        );
                    }
                    Err(e) => {
                        eprintln!("[trader] place_order error: {e}");
                    }
                }
            }
        }
    }
}

// ---------- script engine ----------

fn default_script_text() -> String {
    r#"
// Example script:
//
//  - Reads ctx map
//  - Tweaks some UI params
//  - Emits a bot signal when mid > 4000
//
// Everything is optional; if you don't set a variable, the GUI keeps its own value.

let mid      = ctx["mid"];
let tf_now   = ctx["tf"];
let history_now = ctx["history"];

if mid > 0.0 {
    // make things a bit denser in replay/live
    tf       = tf_now;       // keep same tf
    history  = history_now;  // keep same history

    auto_y        = true;
    show_depth    = true;
    show_ladders  = true;
    show_trades   = true;
    show_volume   = true;
    reload_secs   = 3.0;

    if ctx["mode"] == "live" && mid > 4000.0 {
        bot_signal  = "BUY";
        bot_size    = 0.01;
        bot_comment = "mid > 4000";
    } else {
        bot_signal = "FLAT";
        bot_size   = 0.0;
    }
}
"#
    .to_string()
}

// ---------- main app ----------

struct ComboApp {
    // data
    base_dir: String,
    tickers: Vec<String>,
    data: HashMap<String, TickerData>,

    // mode + time
    mode: Mode,
    time_mode: TimeDisplayMode,
    current_ticker: String,
    live_ts: u64,
    replay_ts: u64,

    // chart/layout
    chart: ChartSettings,
    layout: LayoutSettings,
    reload_interval_secs: f64,
    last_reload: Instant,

    // trading
    trade_tx: mpsc::Sender<TradeCmd>,
    trade_size_input: f64,
    trade_leverage_input: f64,
    last_order_msg: String,
    bot_auto_trade: bool,

    // script engine
    engine: Engine,
    scope: Scope<'static>,
    script_text: String,
    script_auto_run: bool,
    last_script_error: String,
    bot_last_signal: String,
    bot_last_size: f64,
    bot_last_comment: String,
}

impl ComboApp {
    fn new(
        base_dir: String,
        data: HashMap<String, TickerData>,
        tickers: Vec<String>,
        trade_tx: mpsc::Sender<TradeCmd>,
    ) -> Self {
        let current_ticker = "ETH-USD".to_string();

        let live_ts = data
            .get(&current_ticker)
            .map(|td| td.max_ts)
            .unwrap_or_else(now_unix);

        let replay_ts = live_ts;

        let mut engine = Engine::new();
        // (No custom funcs needed yet; ctx is a plain map)

        let scope = Scope::new();

        Self {
            base_dir,
            tickers,
            data,
            mode: Mode::Live,
            time_mode: TimeDisplayMode::Local,
            current_ticker,
            live_ts,
            replay_ts,
            chart: ChartSettings::default(),
            layout: LayoutSettings::default(),
            reload_interval_secs: 3.0,
            last_reload: Instant::now(),
            trade_tx,
            trade_size_input: 0.01,
            trade_leverage_input: 5.0,
            last_order_msg: String::new(),
            bot_auto_trade: false,
            engine,
            scope,
            script_text: default_script_text(),
            script_auto_run: false,
            last_script_error: String::new(),
            bot_last_signal: String::from("FLAT"),
            bot_last_size: 0.0,
            bot_last_comment: String::new(),
        }
    }

    fn current_ticker_data(&self) -> Option<&TickerData> {
        self.data.get(&self.current_ticker)
    }

    fn current_ts(&self) -> Option<u64> {
        match self.mode {
            Mode::Live => Some(self.live_ts),
            Mode::Replay => Some(self.replay_ts),
        }
    }

    fn reload_all_tickers(&mut self) {
        for tk in &self.tickers {
            if let Some(td) = load_ticker_data(&self.base_dir, tk) {
                self.data.insert(tk.clone(), td);
            }
        }

        // keep live_ts / replay_ts in range for current ticker
        if let Some(td) = self.current_ticker_data() {
            if self.live_ts < td.min_ts || self.live_ts > td.max_ts {
                self.live_ts = td.max_ts;
            }
            if self.replay_ts < td.min_ts {
                self.replay_ts = td.min_ts;
            }
            if self.replay_ts > td.max_ts {
                self.replay_ts = td.max_ts;
            }
        }
    }

    fn maybe_reload_csvs(&mut self) {
        let interval =
            Duration::from_secs_f64(self.reload_interval_secs.max(1.0));
        if self.last_reload.elapsed() >= interval {
            self.reload_all_tickers();
            self.last_reload = Instant::now();

            // in Live mode, auto-follow tail
            if let Some(td) = self.current_ticker_data() {
                self.live_ts = td.max_ts;
            }
        }
    }

    fn run_script(&mut self, snap: &Snapshot) {
        self.last_script_error.clear();

        // clear and rebuild scope
        self.scope.clear();

        let mut ctx = RhaiMap::new();
        ctx.insert("mid".into(), snap.last_mid.into());
        ctx.insert("last_vol".into(), snap.last_vol.into());
        ctx.insert(
            "tf".into(),
            (self.chart.selected_tf as i64).into(),
        );
        ctx.insert(
            "history".into(),
            (self.chart.show_candles as i64).into(),
        );
        ctx.insert("auto_y".into(), self.chart.auto_y.into());
        ctx.insert("y_min".into(), self.chart.y_min.into());
        ctx.insert("y_max".into(), self.chart.y_max.into());
        ctx.insert("show_depth".into(), self.layout.show_depth.into());
        ctx.insert(
            "show_ladders".into(),
            self.layout.show_ladders.into(),
        );
        ctx.insert("show_trades".into(), self.layout.show_trades.into());
        ctx.insert("show_volume".into(), self.layout.show_volume.into());
        ctx.insert(
            "reload_secs".into(),
            self.reload_interval_secs.into(),
        );
        ctx.insert(
            "mode".into(),
            match self.mode {
                Mode::Live => "live",
                Mode::Replay => "replay",
            }
            .into(),
        );
        ctx.insert("ticker".into(), self.current_ticker.clone().into());

        self.scope.push("ctx", ctx);

        // outputs with defaults
        self.scope
            .set_value("tf", self.chart.selected_tf as i64);
        self.scope
            .set_value("history", self.chart.show_candles as i64);
        self.scope.set_value("auto_y", self.chart.auto_y);
        self.scope.set_value("y_min", self.chart.y_min);
        self.scope.set_value("y_max", self.chart.y_max);
        self.scope
            .set_value("show_depth", self.layout.show_depth);
        self.scope
            .set_value("show_ladders", self.layout.show_ladders);
        self.scope
            .set_value("show_trades", self.layout.show_trades);
        self.scope
            .set_value("show_volume", self.layout.show_volume);
        self.scope
            .set_value("reload_secs", self.reload_interval_secs);
        self.scope
            .set_value("bot_signal", self.bot_last_signal.clone());
        self.scope
            .set_value("bot_size", self.bot_last_size);
        self.scope
            .set_value("bot_comment", self.bot_last_comment.clone());

        let script = self.script_text.trim();
        if script.is_empty() {
            return;
        }

        if let Err(e) =
            self.engine.eval_with_scope::<rhai::Dynamic>(&mut self.scope, script)
        {
            self.last_script_error = format!("{e}");
            return;
        }

        if let Ok(tf_val) = self.scope.get_value::<i64>("tf") {
            if tf_val > 0 {
                self.chart.selected_tf = tf_val as u64;
            }
        }
        if let Ok(history) = self.scope.get_value::<i64>("history") {
            if history >= 10 && history <= 5000 {
                self.chart.show_candles = history as usize;
            }
        }
        if let Ok(auto_y) = self.scope.get_value::<bool>("auto_y") {
            self.chart.auto_y = auto_y;
        }
        if let Ok(y_min) = self.scope.get_value::<f64>("y_min") {
            self.chart.y_min = y_min;
        }
        if let Ok(y_max) = self.scope.get_value::<f64>("y_max") {
            self.chart.y_max = y_max;
        }
        if let Ok(show_depth) = self.scope.get_value::<bool>("show_depth") {
            self.layout.show_depth = show_depth;
        }
        if let Ok(show_ladders) =
            self.scope.get_value::<bool>("show_ladders")
        {
            self.layout.show_ladders = show_ladders;
        }
        if let Ok(show_trades) =
            self.scope.get_value::<bool>("show_trades")
        {
            self.layout.show_trades = show_trades;
        }
        if let Ok(show_volume) =
            self.scope.get_value::<bool>("show_volume")
        {
            self.layout.show_volume = show_volume;
        }
        if let Ok(reload_secs) =
            self.scope.get_value::<f64>("reload_secs")
        {
            if reload_secs >= 1.0 && reload_secs <= 600.0 {
                self.reload_interval_secs = reload_secs;
            }
        }
        if let Ok(sig) = self.scope.get_value::<String>("bot_signal") {
            self.bot_last_signal = sig;
        }
        if let Ok(size) = self.scope.get_value::<f64>("bot_size") {
            self.bot_last_size = size.max(0.0);
        }
        if let Ok(comment) =
            self.scope.get_value::<String>("bot_comment")
        {
            self.bot_last_comment = comment;
        }
    }

    // ---------- UI pieces ----------

    fn ui_top_bar(&mut self, ui: &mut egui::Ui) {
        // precompute range + display ts WITHOUT mutating self
        let (range_min, range_max, current_ts_display) =
            if let Some(td) = self.current_ticker_data() {
                let ts_now = self.current_ts().unwrap_or(td.max_ts);
                (Some(td.min_ts), Some(td.max_ts), Some(ts_now))
            } else {
                (None, None, None)
            };

        ui.horizontal(|ui| {
            // mode
            ui.label("Mode:");
            if ui
                .selectable_label(self.mode == Mode::Live, "Live")
                .clicked()
            {
                self.mode = Mode::Live;
            }
            if ui
                .selectable_label(self.mode == Mode::Replay, "Replay")
                .clicked()
            {
                self.mode = Mode::Replay;
            }

            ui.separator();

            // ticker dropdown
            let tickers = self.tickers.clone();
            ui.menu_button(
                format!("Ticker: {}", self.current_ticker),
                |ui| {
                    for t in &tickers {
                        let selected = *t == self.current_ticker;
                        if ui.selectable_label(selected, t).clicked() {
                            self.current_ticker = t.clone();
                            ui.close_menu();
                        }
                    }
                },
            );

            ui.separator();

            // time mode
            ui.label("Time:");
            for mode in [TimeDisplayMode::Local, TimeDisplayMode::Unix] {
                if ui
                    .selectable_label(self.time_mode == mode, mode.label())
                    .clicked()
                {
                    self.time_mode = mode;
                }
            }

            // range + current ts
            if let (Some(min_ts), Some(max_ts)) = (range_min, range_max) {
                ui.separator();
                ui.label(format!(
                    "Range: {} → {}",
                    format_ts(self.time_mode, min_ts),
                    format_ts(self.time_mode, max_ts)
                ));

                if let Some(ts_now) = current_ts_display {
                    ui.separator();
                    ui.label(format!(
                        "Current ts: {}",
                        format_ts(self.time_mode, ts_now)
                    ));
                }
            } else {
                ui.separator();
                ui.label("No CSV data for this ticker yet.");
            }
        });

        ui.separator();

        // replay slider OR live tail follow
        if matches!(self.mode, Mode::Replay) {
            if let (Some(min_ts), Some(max_ts)) = (range_min, range_max) {
                let mut ts = self.replay_ts.clamp(min_ts, max_ts);
                ui.horizontal(|ui| {
                    ui.label("Replay time:");
                    ui.add(
                        egui::Slider::new(&mut ts, min_ts..=max_ts)
                            .show_value(false)
                            .text("ts"),
                    );
                    if ui.button("◀").clicked() && ts > min_ts {
                        ts -= 1;
                    }
                    if ui.button("▶").clicked() && ts < max_ts {
                        ts += 1;
                    }
                    if ui.button("Now").clicked() {
                        ts = max_ts;
                    }
                    ui.label(format_ts(self.time_mode, ts));
                });
                self.replay_ts = ts;
            } else {
                ui.label("Replay: no data.");
            }

            ui.separator();
        } else {
            if let Some(max_ts) = range_max {
                self.live_ts = max_ts;
                if self.replay_ts < max_ts {
                    self.replay_ts = max_ts;
                }
            }
        }

        // chart + layout controls
        ui.horizontal(|ui| {
            ui.label("History candles:");
            ui.add(
                egui::Slider::new(&mut self.chart.show_candles, 20..=2000)
                    .logarithmic(true),
            );

            ui.separator();
            ui.label("TF (seconds):");
            ui.add(
                egui::DragValue::new(&mut self.chart.selected_tf)
                    .clamp_range(1..=86_400)
                    .speed(1),
            );

            ui.separator();
            ui.checkbox(&mut self.chart.auto_y, "Auto Y");
            if !self.chart.auto_y {
                ui.label("Y range:");
                ui.add(
                    egui::DragValue::new(&mut self.chart.y_min)
                        .speed(1.0)
                        .prefix("min "),
                );
                ui.add(
                    egui::DragValue::new(&mut self.chart.y_max)
                        .speed(1.0)
                        .prefix("max "),
                );
            }

            ui.separator();
            ui.checkbox(&mut self.layout.show_depth, "Depth");
            ui.checkbox(&mut self.layout.show_ladders, "Ladders");
            ui.checkbox(&mut self.layout.show_trades, "Trades");
            ui.checkbox(&mut self.layout.show_volume, "Volume");

            ui.separator();
            ui.label("Reload (s):");
            ui.add(
                egui::DragValue::new(&mut self.reload_interval_secs)
                    .speed(0.5)
                    .clamp_range(1.0..=600.0),
            );
        });

        ui.separator();

        ui.horizontal(|ui| {
            ui.checkbox(&mut self.script_auto_run, "Auto-run script");
            if ui.button("Run script now").clicked() {
                if let (Some(td), Some(ts)) =
                    (self.current_ticker_data(), self.current_ts())
                {
                    let snap =
                        compute_snapshot_for(td, ts, self.chart.selected_tf);
                    self.run_script(&snap);
                }
            }

            ui.separator();
            ui.checkbox(&mut self.bot_auto_trade, "Bot auto-trade");

            if !self.bot_last_signal.is_empty() {
                ui.separator();
                ui.label(format!(
                    "Bot signal: {} size {:.6}  {}",
                    self.bot_last_signal, self.bot_last_size, self.bot_last_comment
                ));
            }
        });

        if !self.last_script_error.is_empty() {
            ui.colored_label(
                Color32::RED,
                format!("Script error: {}", self.last_script_error),
            );
        }

        ui.separator();
    }

    fn ui_depth_plot(
        &self,
        ui: &mut egui::Ui,
        snap: &Snapshot,
        height: f32,
    ) {
        let mut bid_points = Vec::new();
        let mut ask_points = Vec::new();

        let mut cum = 0.0;
        for (k, s) in snap.bids.iter().rev() {
            let p = key_to_price(*k);
            cum += s;
            bid_points.push((p, cum));
        }

        cum = 0.0;
        for (k, s) in snap.asks.iter() {
            let p = key_to_price(*k);
            cum += s;
            ask_points.push((p, cum));
        }

        Plot::new("depth_plot")
            .height(height)
            .show(ui, |plot_ui| {
                if !bid_points.is_empty() {
                    let pts: PlotPoints = bid_points
                        .iter()
                        .map(|(x, y)| [*x, *y])
                        .collect::<Vec<_>>()
                        .into();
                    plot_ui.line(Line::new(pts).name("Bids"));
                }
                if !ask_points.is_empty() {
                    let pts: PlotPoints = ask_points
                        .iter()
                        .map(|(x, y)| [*x, *y])
                        .collect::<Vec<_>>()
                        .into();
                    plot_ui.line(Line::new(pts).name("Asks"));
                }
            });
    }

    fn ui_ladders_and_trades(
        &self,
        ui: &mut egui::Ui,
        snap: &Snapshot,
        height: f32,
    ) {
        ui.vertical(|ui| {
            if self.layout.show_ladders {
                ui.label("Top 20 ladder:");
                ui.columns(2, |cols| {
                    cols[0].label("Bids");
                    egui::Grid::new("bids_grid")
                        .striped(true)
                        .show(&mut cols[0], |ui| {
                            ui.label("Price");
                            ui.label("Size");
                            ui.end_row();
                            for (k, s) in snap.bids.iter().rev().take(20) {
                                let p = key_to_price(*k);
                                ui.label(format!("{:>9.2}", p));
                                ui.label(format!("{:>10.4}", s));
                                ui.end_row();
                            }
                        });

                    cols[1].label("Asks");
                    egui::Grid::new("asks_grid")
                        .striped(true)
                        .show(&mut cols[1], |ui| {
                            ui.label("Price");
                            ui.label("Size");
                            ui.end_row();
                            for (k, s) in snap.asks.iter().take(20) {
                                let p = key_to_price(*k);
                                ui.label(format!("{:>9.2}", p));
                                ui.label(format!("{:>10.4}", s));
                                ui.end_row();
                            }
                        });
                });

                ui.add_space(4.0);
            }

            if self.layout.show_trades {
                ui.separator();
                ui.label("Recent trades:");
                egui::ScrollArea::vertical()
                    .max_height(height * 0.5)
                    .show(ui, |ui| {
                        egui::Grid::new("trades_grid")
                            .striped(true)
                            .show(ui, |ui| {
                                ui.label("Time");
                                ui.label("Side");
                                ui.label("Size");
                                ui.label("Source");
                                ui.end_row();

                                for tr in snap.trades.iter().rev() {
                                    ui.label(format_ts(self.time_mode, tr.ts));
                                    ui.label(&tr.side);
                                    ui.label(&tr.size_str);
                                    ui.label(&tr.source);
                                    ui.end_row();
                                }
                            });
                    });
            }
        });
    }

    fn ui_trading_panel(&mut self, ui: &mut egui::Ui) {
        ui.group(|ui| {
            ui.heading(
                RichText::new("Testnet Trading")
                    .strong()
                    .color(Color32::from_rgb(200, 230, 255)),
            );
            ui.label("Requires DYDX_TESTNET_MNEMONIC in your shell.");

            ui.add_space(4.0);

            ui.horizontal(|ui| {
                ui.label("Size (units):");
                ui.add(
                    egui::DragValue::new(&mut self.trade_size_input)
                        .speed(0.001)
                        .clamp_range(0.0..=1000.0),
                );
            });

            ui.horizontal(|ui| {
                ui.label("Leverage (hint only):");
                ui.add(
                    egui::DragValue::new(&mut self.trade_leverage_input)
                        .speed(0.5)
                        .clamp_range(1.0..=50.0),
                );
            });

            ui.add_space(4.0);

            ui.horizontal(|ui| {
                if ui.button("Market BUY").clicked() {
                    self.send_manual_order(OrderSide::Buy);
                }
                if ui.button("Market SELL").clicked() {
                    self.send_manual_order(OrderSide::Sell);
                }
            });

            ui.add_space(4.0);

            if !self.bot_last_signal.is_empty() {
                ui.separator();
                ui.label(format!(
                    "Bot: {} size {:.6}  {}",
                    self.bot_last_signal, self.bot_last_size, self.bot_last_comment
                ));
                ui.horizontal(|ui| {
                    if ui.button("Send bot BUY").clicked() {
                        if self.bot_last_signal.to_uppercase().contains("BUY")
                            && self.bot_last_size > 0.0
                        {
                            self.send_bot_order(OrderSide::Buy);
                        }
                    }
                    if ui.button("Send bot SELL").clicked() {
                        if self.bot_last_signal.to_uppercase().contains("SELL")
                            && self.bot_last_size > 0.0
                        {
                            self.send_bot_order(OrderSide::Sell);
                        }
                    }
                });
            }

            if !self.last_order_msg.is_empty() {
                ui.separator();
                ui.label(&self.last_order_msg);
            }
        });
    }

    fn send_manual_order(&mut self, side: OrderSide) {
        let size_val = self.trade_size_input.max(0.0);
        if size_val <= 0.0 {
            self.last_order_msg = "Size must be > 0".to_string();
            return;
        }

        let s_str = format!("{:.8}", size_val);
        let size_bd = match BigDecimal::from_str(&s_str) {
            Ok(bd) => bd,
            Err(e) => {
                self.last_order_msg =
                    format!("Invalid size: {s_str} ({e})");
                return;
            }
        };

        let lev = self.trade_leverage_input.max(1.0);

        if self
            .trade_tx
            .try_send(TradeCmd::Market {
                ticker: self.current_ticker.clone(),
                side,
                size: size_bd.clone(),
                leverage: lev,
            })
            .is_ok()
        {
            self.last_order_msg = format!(
                "Sent {:?} {} size {} lev {} (see terminal)",
                side, self.current_ticker, s_str, lev
            );
        } else {
            self.last_order_msg = "Trade channel full/busy".to_string();
        }
    }

    fn send_bot_order(&mut self, side: OrderSide) {
        let size_val = self.bot_last_size.max(0.0);
        if size_val <= 0.0 {
            self.last_order_msg =
                "Bot size <= 0; nothing to send".to_string();
            return;
        }

        let s_str = format!("{:.8}", size_val);
        let size_bd = match BigDecimal::from_str(&s_str) {
            Ok(bd) => bd,
            Err(e) => {
                self.last_order_msg =
                    format!("Bot size invalid: {s_str} ({e})");
                return;
            }
        };

        let lev = self.trade_leverage_input.max(1.0);

        if self
            .trade_tx
            .try_send(TradeCmd::Market {
                ticker: self.current_ticker.clone(),
                side,
                size: size_bd.clone(),
                leverage: lev,
            })
            .is_ok()
        {
            self.last_order_msg = format!(
                "BOT {:?} {} size {} lev {} (see terminal) :: {}",
                side, self.current_ticker, s_str, lev, self.bot_last_comment
            );
        } else {
            self.last_order_msg =
                "Trade channel full/busy for bot".to_string();
        }
    }

    fn ui_candles_and_volume(
        &mut self,
        ui: &mut egui::Ui,
        snap: &Snapshot,
        is_live: bool,
    ) {
        if snap.candles.is_empty() {
            ui.label(if is_live {
                "No candles yet (waiting for book history...)"
            } else {
                "No candles at this replay time."
            });
            return;
        }

        let series = &snap.candles;
        let len = series.len();
        let window_len = self.chart.show_candles.min(len).max(1);
        let visible = &series[len - window_len..];

        let (y_min, y_max) = if self.chart.auto_y {
            let lo = visible
                .iter()
                .map(|c| c.low)
                .fold(f64::MAX, f64::min);
            let hi = visible
                .iter()
                .map(|c| c.high)
                .fold(f64::MIN, f64::max);
            let span = (hi - lo).max(1e-3);
            let pad = span * 0.05;
            let min_v = lo - pad;
            let max_v = hi + pad;
            self.chart.y_min = min_v;
            self.chart.y_max = max_v;
            (min_v, max_v)
        } else {
            (self.chart.y_min, self.chart.y_max)
        };

        let avail_h = ui.available_height();
        let avail_w = ui.available_width();
        let candles_h = avail_h * 0.7;
        let volume_h = avail_h * 0.3;

        let tf = self.chart.selected_tf as f64;
        let last = visible.last().unwrap();
        let x_center = last.t as f64 + tf * 0.5;
        let base_span = tf * self.chart.show_candles as f64;
        let span = base_span;
        let x_min = x_center - span * 0.5;
        let x_max = x_center + span * 0.5;

        // candles
        ui.allocate_ui(egui::vec2(avail_w, candles_h), |ui| {
            let mode = self.time_mode;
            let plot_resp = Plot::new(if is_live {
                "candles_live"
            } else {
                "candles_replay"
            })
            .height(candles_h)
            .include_y(y_min)
            .include_y(y_max)
            .allow_drag(true)
            .allow_zoom(true)
            .x_axis_formatter(move |mark, _bounds, _transform| {
                let ts = mark.value as u64;
                format_ts(mode, ts)
            })
            .show(ui, |plot_ui| {
                plot_ui.set_plot_bounds(PlotBounds::from_min_max(
                    [x_min, y_min],
                    [x_max, y_max],
                ));

                for c in visible {
                    let left = c.t as f64;
                    let right = left + tf;
                    let mid = left + tf * 0.5;

                    let top = c.open.max(c.close);
                    let bot = c.open.min(c.close);

                    let color = if c.close >= c.open {
                        Color32::GREEN
                    } else {
                        Color32::RED
                    };

                    // wick
                    let wick_pts: PlotPoints =
                        vec![[mid, c.low], [mid, c.high]].into();
                    plot_ui.line(Line::new(wick_pts).color(color));

                    // filled body
                    let body_pts: PlotPoints = vec![
                        [left, bot],
                        [left, top],
                        [right, top],
                        [right, bot],
                        [left, bot],
                    ]
                    .into();
                    plot_ui.line(
                        Line::new(body_pts).color(color).width(2.0),
                    );
                }

                let now_x = if is_live {
                    self.live_ts as f64
                } else {
                    self.replay_ts as f64
                };
                plot_ui.vline(VLine::new(now_x).name("now_ts"));
            });

            // vertical zoom: Shift + scroll over candles plot
            let hovered = plot_resp.response.hovered();
            let mut scroll_y = 0.0f32;
            let mut shift = false;
            ui.ctx().input(|i| {
                scroll_y = i.raw_scroll_delta.y;
                shift = i.modifiers.shift;
            });
            if hovered && shift && scroll_y != 0.0 {
                self.chart.auto_y = false;
                let factor = 1.0 + (scroll_y as f64 * 0.002);
                let factor = factor.clamp(0.2, 5.0);
                let center =
                    (self.chart.y_min + self.chart.y_max) * 0.5;
                let half_span = (self.chart.y_max - self.chart.y_min)
                    .max(1e-6)
                    * factor
                    * 0.5;
                self.chart.y_min = center - half_span;
                self.chart.y_max = center + half_span;
            }
        });

        ui.separator();

        if self.layout.show_volume {
            ui.allocate_ui(egui::vec2(avail_w, volume_h), |ui| {
                let mode = self.time_mode;
                let plot_resp = Plot::new(if is_live {
                    "volume_live"
                } else {
                    "volume_replay"
                })
                .height(volume_h)
                .include_y(0.0)
                .allow_drag(true)
                .allow_zoom(true)
                .x_axis_formatter(move |mark, _bounds, _transform| {
                    let ts = mark.value as u64;
                    format_ts(mode, ts)
                })
                .show(ui, |plot_ui| {
                    let max_vol = visible
                        .iter()
                        .map(|c| c.volume)
                        .fold(0.0_f64, f64::max)
                        .max(1e-6);
                    let y_max_v = max_vol * 1.1;

                    plot_ui.set_plot_bounds(PlotBounds::from_min_max(
                        [x_min, 0.0],
                        [x_max, y_max_v],
                    ));

                    for c in visible {
                        let left = c.t as f64;
                        let mid = left + tf * 0.5;
                        let color =
                            Color32::from_rgb(120, 170, 240);

                        let line_pts: PlotPoints =
                            vec![[mid, 0.0], [mid, c.volume]].into();
                        plot_ui
                            .line(Line::new(line_pts).color(color).width(2.0));
                    }
                });

                // we reuse same vertical zoom as candles for price, so no special logic here
                let _ = plot_resp;
            });
        }
    }

    fn ui_script_engine(
        &mut self,
        ui: &mut egui::Ui,
        _snap: Option<&Snapshot>,
    ) {
        ui.heading("Script Engine (Rhai)");
        ui.label("Variables: ctx, tf, history, auto_y, y_min, y_max, show_*, reload_secs, bot_*");

        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.add(
                egui::TextEdit::multiline(&mut self.script_text)
                    .code_editor()
                    .desired_rows(12),
            );
        });
    }

    fn ui_main_view(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        let avail = ui.available_size();
        egui::ScrollArea::both()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.heading(format!(
                    "{} {} @ {}",
                    match self.mode {
                        Mode::Live => "LIVE",
                        Mode::Replay => "REPLAY",
                    },
                    self.current_ticker,
                    format_ts(
                        self.time_mode,
                        match self.mode {
                            Mode::Live => self.live_ts,
                            Mode::Replay => self.replay_ts,
                        }
                    )
                ));

                ui.add_space(4.0);

                let depth_height = avail.y * 0.35;

                ui.horizontal(|ui| {
                    if self.layout.show_depth {
                        ui.allocate_ui(
                            egui::vec2(avail.x * 0.4, depth_height),
                            |ui| {
                                self.ui_depth_plot(ui, snap, depth_height);
                            },
                        );
                    }

                    ui.allocate_ui(
                        egui::vec2(avail.x * 0.6, depth_height),
                        |ui| {
                            ui.horizontal(|cols| {
                                cols.vertical(|ui| {
                                    self
                                        .ui_ladders_and_trades(
                                            ui,
                                            snap,
                                            depth_height,
                                        );
                                });
                                cols.add_space(8.0);
                                cols.vertical(|ui| {
                                    self.ui_trading_panel(ui);
                                });
                            });
                        },
                    );
                });

                ui.separator();

                self.ui_candles_and_volume(
                    ui,
                    snap,
                    matches!(self.mode, Mode::Live),
                );
            });
    }
}

impl eframe::App for ComboApp {
    fn update(
        &mut self,
        ctx: &egui::Context,
        _frame: &mut eframe::Frame,
    ) {
        self.maybe_reload_csvs();

        let current_snap = if let (Some(td), Some(ts)) =
            (self.current_ticker_data(), self.current_ts())
        {
            Some(compute_snapshot_for(
                td,
                ts,
                self.chart.selected_tf,
            ))
        } else {
            None
        };

        if self.script_auto_run {
            if let Some(ref snap) = current_snap {
                self.run_script(snap);
            }
        }

        let snap_ref = current_snap.as_ref();

        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            self.ui_top_bar(ui);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(snap) = snap_ref {
                self.ui_main_view(ui, snap);
            } else {
                ui.label(
                    "No data yet. Make sure the daemon is running and writing CSVs into ./data.",
                );
            }
        });

        egui::TopBottomPanel::bottom("script_panel")
            .resizable(true)
            .default_height(200.0)
            .show(ctx, |ui| {
                self.ui_script_engine(ui, snap_ref);
            });

        ctx.request_repaint_after(Duration::from_millis(200));
    }
}

// ---------- main ----------

fn main() {
    init_crypto_provider();

    let base_dir = "data".to_string();
    let tickers = vec![
        "ETH-USD".to_string(),
        "BTC-USD".to_string(),
        "SOL-USD".to_string(),
    ];

    let mut data = HashMap::new();
    for tk in &tickers {
        if let Some(td) = load_ticker_data(&base_dir, tk) {
            data.insert(tk.clone(), td);
        }
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let (trade_tx, trade_rx) = mpsc::channel::<TradeCmd>(32);

    // spawn trader
    rt.spawn(run_trader(trade_rx));

    let app = ComboApp::new(base_dir, data, tickers, trade_tx);

    let options = eframe::NativeOptions::default();
    if let Err(e) = eframe::run_native(
        "dYdX CSV Live+Replay + Script Engine",
        options,
        Box::new(|_cc| Box::new(app)),
    ) {
        eprintln!("eframe error: {e}");
    }

    drop(rt);
}
