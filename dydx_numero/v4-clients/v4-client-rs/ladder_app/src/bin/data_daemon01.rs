// ladder_app/src/bin/data_daemon.rs
//
// Headless data collector for dYdX v4 testnet.
// - Subscribes to orderbook feeds for ETH-USD / BTC-USD / SOL-USD
// - Maintains in-memory books and CandleAggs for many TFs (1s..1d)
// - Writes:
//     data/orderbook_{TICKER}.csv
//     data/candles_{TICKER}_{TF}.csv
//   All timestamped.
// - Designed to run 24/7 as a background process / daemon.
//
// The existing GUI (full_gui11.rs) can then just *read* these CSVs
// and display history + current state.

mod candle_agg;

use candle_agg::{Candle, CandleAgg};

use std::collections::{BTreeMap, HashMap};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::time::sleep;

// dYdX client
use dydx_client::config::ClientConfig;
use dydx_client::indexer::{
    Feed as DxFeed, Feeds, IndexerClient, OrderbookResponsePriceLevel, OrdersMessage, Ticker,
};

// ---------- shared timeframe config (same idea as full_gui11) ----------

const TF_CHOICES: &[u64] = &[
    // seconds
    1, 5, 10, 15, 30,
    // minutes
    60,        // 1m
    120,       // 2m
    180,       // 3m
    300,       // 5m
    600,       // 10m
    900,       // 15m
    1800,      // 30m
    // hours
    3600,      // 1h
    7200,      // 2h
    14400,     // 4h
    28800,     // 8h
    43200,     // 12h
    86400,     // 1d
];

// ---------- basic helpers ----------

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

// ---------- CSV writers (same directory / similar style as full_gui11) ----------

fn ensure_data_dir() {
    let dir = Path::new("data");
    let _ = std::fs::create_dir_all(dir);
}

fn append_book_csv(ticker: &str, kind: &str, side: &str, price: f64, size: f64) {
    ensure_data_dir();
    let ts = now_unix();
    let path = Path::new("data").join(format!("orderbook_{ticker}.csv"));

    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{ts},{ticker},{kind},{side},{price},{size}");
    }
}

fn append_candle_csv(ticker: &str, tf_secs: u64, c: &Candle) {
    ensure_data_dir();
    // candles_{TICKER}_{TF}.csv
    // ts_open,tf_sec,open,high,low,close,volume
    let path = Path::new("data").join(format!("candles_{}_{}.csv", ticker, tf_secs));

    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(
            f,
            "{},{},{},{},{},{},{}",
            c.t, tf_secs, c.open, c.high, c.low, c.close, c.volume
        );
    }
}

// ---------- simple book state (per ticker) ----------

#[derive(Default)]
struct BookState {
    bids: BTreeMap<PriceKey, f64>,
    asks: BTreeMap<PriceKey, f64>,
}

impl BookState {
    fn apply_initial(
        &mut self,
        ticker: &str,
        bids: Vec<OrderbookResponsePriceLevel>,
        asks: Vec<OrderbookResponsePriceLevel>,
    ) {
        self.bids.clear();
        self.asks.clear();

        for lvl in bids {
            let p = lvl.price.0.to_string().parse::<f64>().unwrap_or(0.0);
            let s = lvl.size.0.to_string().parse::<f64>().unwrap_or(0.0);
            let key = price_to_key(p);
            if s != 0.0 {
                self.bids.insert(key, s);
            }
            append_book_csv(ticker, "book_init", "bid", p, s);
        }

        for lvl in asks {
            let p = lvl.price.0.to_string().parse::<f64>().unwrap_or(0.0);
            let s = lvl.size.0.to_string().parse::<f64>().unwrap_or(0.0);
            let key = price_to_key(p);
            if s != 0.0 {
                self.asks.insert(key, s);
            }
            append_book_csv(ticker, "book_init", "ask", p, s);
        }
    }

    fn apply_update(
        &mut self,
        ticker: &str,
        bids: Option<Vec<OrderbookResponsePriceLevel>>,
        asks: Option<Vec<OrderbookResponsePriceLevel>>,
    ) {
        if let Some(bv) = bids {
            for lvl in bv {
                let p = lvl.price.0.to_string().parse::<f64>().unwrap_or(0.0);
                let s = lvl.size.0.to_string().parse::<f64>().unwrap_or(0.0);
                let key = price_to_key(p);

                if s == 0.0 {
                    self.bids.remove(&key);
                } else {
                    self.bids.insert(key, s);
                }
                append_book_csv(ticker, "delta", "bid", p, s);
            }
        }

        if let Some(av) = asks {
            for lvl in av {
                let p = lvl.price.0.to_string().parse::<f64>().unwrap_or(0.0);
                let s = lvl.size.0.to_string().parse::<f64>().unwrap_or(0.0);
                let key = price_to_key(p);

                if s == 0.0 {
                    self.asks.remove(&key);
                } else {
                    self.asks.insert(key, s);
                }
                append_book_csv(ticker, "delta", "ask", p, s);
            }
        }
    }

    fn mid(&self) -> Option<f64> {
        let bp = self.bids.iter().next_back();
        let ap = self.asks.iter().next();
        match (bp, ap) {
            (Some((b, _)), Some((a, _))) => {
                let pb = key_to_price(*b);
                let pa = key_to_price(*a);
                Some((pb + pa) * 0.5)
            }
            _ => None,
        }
    }
}

// ---------- TLS provider (same trick as GUI) ----------

fn init_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

// ---------- per-ticker daemon task ----------

async fn run_ticker_daemon(
    indexer_cfg: dydx_client::config::IndexerConfig,
    ticker_str: String,
) {
    loop {
        eprintln!("[daemon] starting feed for {ticker_str}");

        // fresh client each reconnect
        let mut indexer = IndexerClient::new(indexer_cfg.clone());

        let mut feeds: Feeds<'_> = indexer.feed();
        let ticker = Ticker(ticker_str.clone());

        let mut feed: DxFeed<OrdersMessage> = match feeds.orders(&ticker, false).await {
            Ok(f) => f,
            Err(e) => {
                eprintln!("[daemon] orders feed error for {ticker_str}: {e}");
                sleep(Duration::from_secs(5)).await;
                continue;
            }
        };

        let mut book = BookState::default();

        // Candle aggregators for all TFs.
        let mut aggs: HashMap<u64, CandleAgg> = HashMap::new();
        let mut last_logged: HashMap<u64, u64> = HashMap::new();
        for tf in TF_CHOICES {
            aggs.insert(*tf, CandleAgg::new(*tf));
            last_logged.insert(*tf, 0);
        }

        while let Some(msg) = feed.recv().await {
            let ts = now_unix();

            match msg {
                OrdersMessage::Initial(init) => {
                    book.apply_initial(&ticker_str, init.contents.bids, init.contents.asks);
                }
                OrdersMessage::Update(upd) => {
                    book.apply_update(&ticker_str, upd.contents.bids, upd.contents.asks);
                }
            }

            // derive mid price & feed into candles
            if let Some(mid) = book.mid() {
                let vol = 0.0_f64; // placeholder volume — can refine later

                for (tf, agg) in aggs.iter_mut() {
                    agg.update(ts, mid, vol);

                    if let Some(last) = agg.series().last() {
                        let entry = last_logged.entry(*tf).or_insert(0);
                        if *entry != last.t {
                            // new completed candle for this TF
                            append_candle_csv(&ticker_str, *tf, last);
                            *entry = last.t;
                        }
                    }
                }
            }
        }

        eprintln!("[daemon] feed for {ticker_str} ended, reconnecting in 5s...");
        sleep(Duration::from_secs(5)).await;
    }
}

// ---------- main ----------

#[tokio::main]
async fn main() {
    init_crypto_provider();

    let config = match ClientConfig::from_file("client/tests/testnet.toml").await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[daemon] Failed to load client/tests/testnet.toml: {e}");
            return;
        }
    };

    let indexer_cfg = config.indexer.clone();

    let tickers = ["ETH-USD", "BTC-USD", "SOL-USD"];

    for tk in tickers {
        let cfg = indexer_cfg.clone();
        let ticker_str = tk.to_string();
        tokio::spawn(async move {
            run_ticker_daemon(cfg, ticker_str).await;
        });
    }

    // park forever
    loop {
        sleep(Duration::from_secs(3600)).await;
    }
}
