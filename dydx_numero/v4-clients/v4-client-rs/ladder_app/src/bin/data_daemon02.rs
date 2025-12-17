// ladder_app/src/bin/data_daemon01.rs
//
// Headless data daemon for dYdX v4 testnet.
//
// - Subscribes to L2 orders feed for multiple tickers
// - Maintains an in-memory orderbook per ticker
// - Derives mid-price candles (30s/1m/3m/5m)
// - Appends everything to CSVs under ./data:
//     data/orderbook_{TICKER}.csv
//         ts,ticker,kind,side,price,size
//         kind ∈ {book_init,delta}
//     data/candles_{TICKER}_{TF}.csv
//         open_ts,ticker,tf_secs,open,high,low,close,volume
//
// This is meant to run 24/7 (via launchd), while your GUI only *reads* the data.

mod candle_agg;

use candle_agg::{Candle, CandleAgg};

use std::collections::BTreeMap;
use std::fs::{create_dir_all, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::time::sleep;

// dYdX client config + indexer
use dydx_client::config::ClientConfig;
use dydx_client::indexer::{
    Feed as DxFeed, Feeds, IndexerClient, IndexerConfig, OrderbookResponsePriceLevel,
    OrdersMessage, Ticker,
};

// ---------- basic helpers ----------

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_secs()
}

// integer price key for stable BTreeMap ordering
type PriceKey = i64;

fn price_to_key(price: f64) -> PriceKey {
    (price * 10_000.0).round() as PriceKey
}

fn key_to_price(key: PriceKey) -> f64 {
    key as f64 / 10_000.0
}

// ---------- CSV writers (compatible with full_gui / replay) ----------

fn ensure_data_dir() {
    let _ = create_dir_all(Path::new("data"));
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
    let path =
        Path::new("data").join(format!("candles_{}_{}s.csv", ticker.replace('-', "_"), tf_secs));
    let open_ts = c.t;

    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(
            f,
            "{open_ts},{ticker},{tf_secs},{},{},{},{},{}",
            c.open, c.high, c.low, c.close, c.volume
        );
    }
}

// ---------- orderbook state ----------

#[derive(Default, Clone, Debug)]
struct DaemonBook {
    bids: BTreeMap<PriceKey, f64>,
    asks: BTreeMap<PriceKey, f64>,
}

impl DaemonBook {
    fn apply_initial(
        &mut self,
        bids: Vec<OrderbookResponsePriceLevel>,
        asks: Vec<OrderbookResponsePriceLevel>,
        ticker: &str,
    ) {
        self.bids.clear();
        self.asks.clear();

        for lvl in bids {
            let p = lvl
                .price
                .0
                .to_string()
                .parse::<f64>()
                .unwrap_or(0.0);
            let s = lvl
                .size
                .0
                .to_string()
                .parse::<f64>()
                .unwrap_or(0.0);
            let key = price_to_key(p);
            if s != 0.0 {
                self.bids.insert(key, s);
            }
            append_book_csv(ticker, "book_init", "bid", p, s);
        }

        for lvl in asks {
            let p = lvl
                .price
                .0
                .to_string()
                .parse::<f64>()
                .unwrap_or(0.0);
            let s = lvl
                .size
                .0
                .to_string()
                .parse::<f64>()
                .unwrap_or(0.0);
            let key = price_to_key(p);
            if s != 0.0 {
                self.asks.insert(key, s);
            }
            append_book_csv(ticker, "book_init", "ask", p, s);
        }
    }

    fn apply_levels(
        map: &mut BTreeMap<PriceKey, f64>,
        levels: Vec<OrderbookResponsePriceLevel>,
        side: &str,
        ticker: &str,
    ) {
        for lvl in levels {
            let p = lvl
                .price
                .0
                .to_string()
                .parse::<f64>()
                .unwrap_or(0.0);
            let s = lvl
                .size
                .0
                .to_string()
                .parse::<f64>()
                .unwrap_or(0.0);
            let key = price_to_key(p);

            if s == 0.0 {
                map.remove(&key);
            } else {
                map.insert(key, s);
            }

            append_book_csv(ticker, "delta", side, p, s);
        }
    }

    fn apply_update(
        &mut self,
        bids: Option<Vec<OrderbookResponsePriceLevel>>,
        asks: Option<Vec<OrderbookResponsePriceLevel>>,
        ticker: &str,
    ) {
        if let Some(b) = bids {
            Self::apply_levels(&mut self.bids, b, "bid", ticker);
        }
        if let Some(a) = asks {
            Self::apply_levels(&mut self.asks, a, "ask", ticker);
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

// ---------- TLS provider init ----------

fn init_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

// ---------- per-ticker daemon ----------

async fn run_market_daemon(indexer_cfg: IndexerConfig, ticker_str: String) {
    let mut indexer = IndexerClient::new(indexer_cfg);

    loop {
        eprintln!("[daemon] subscribing orders for {ticker_str}");

        let mut feeds: Feeds<'_> = indexer.feed();
        let ticker = Ticker(ticker_str.clone());

        let mut feed: DxFeed<OrdersMessage> = match feeds.orders(&ticker, false).await {
            Ok(f) => f,
            Err(e) => {
                eprintln!(
                    "[daemon] orders feed error for {ticker_str}: {e}; retrying in 5s"
                );
                sleep(Duration::from_secs(5)).await;
                continue;
            }
        };

        let mut book = DaemonBook::default();

        let mut tf_30s = CandleAgg::new(30);
        let mut tf_1m = CandleAgg::new(60);
        let mut tf_3m = CandleAgg::new(180);
        let mut tf_5m = CandleAgg::new(300);

        // last written candle open_ts per TF to avoid duplicates
        let mut last_30s_written: u64 = 0;
        let mut last_1m_written: u64 = 0;
        let mut last_3m_written: u64 = 0;
        let mut last_5m_written: u64 = 0;

        while let Some(msg) = feed.recv().await {
            match msg {
                OrdersMessage::Initial(init) => {
                    book.apply_initial(
                        init.contents.bids,
                        init.contents.asks,
                        &ticker_str,
                    );
                }
                OrdersMessage::Update(upd) => {
                    book.apply_update(
                        upd.contents.bids,
                        upd.contents.asks,
                        &ticker_str,
                    );
                }
            }

            let ts = now_unix();
            if let Some(mid) = book.mid() {
                // We don't know true traded volume at L3 here, so volume is
                // "book-tick volume" (can be refined later with a trades feed).
                let vol = 0.0;

                tf_30s.update(ts, mid, vol);
                tf_1m.update(ts, mid, vol);
                tf_3m.update(ts, mid, vol);
                tf_5m.update(ts, mid, vol);

                // Flush only *new* completed candles per TF
                if let Some(c) = tf_30s.series().last() {
                    if c.t != last_30s_written {
                        append_candle_csv(&ticker_str, 30, c);
                        last_30s_written = c.t;
                    }
                }
                if let Some(c) = tf_1m.series().last() {
                    if c.t != last_1m_written {
                        append_candle_csv(&ticker_str, 60, c);
                        last_1m_written = c.t;
                    }
                }
                if let Some(c) = tf_3m.series().last() {
                    if c.t != last_3m_written {
                        append_candle_csv(&ticker_str, 180, c);
                        last_3m_written = c.t;
                    }
                }
                if let Some(c) = tf_5m.series().last() {
                    if c.t != last_5m_written {
                        append_candle_csv(&ticker_str, 300, c);
                        last_5m_written = c.t;
                    }
                }
            }
        }

        eprintln!(
            "[daemon] feed ended for {ticker_str}; reconnecting in 5s..."
        );
        sleep(Duration::from_secs(5)).await;
    }
}

// ---------- main ----------

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    init_crypto_provider();

    let config = match ClientConfig::from_file("client/tests/testnet.toml").await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[daemon] failed to load client/tests/testnet.toml: {e}");
            return;
        }
    };

    let indexer_cfg: IndexerConfig = config.indexer.clone();

    let tickers = ["ETH-USD", "BTC-USD", "SOL-USD"];

    for tk in tickers {
        let cfg = indexer_cfg.clone();
        let t = tk.to_string();
        tokio::spawn(async move {
            run_market_daemon(cfg, t).await;
        });
    }

    eprintln!(
        "[daemon] started for tickers: {}",
        tickers.join(", ")
    );
    eprintln!("[daemon] writing CSVs under ./data/ ...");

    // Just park the main task forever; the market tasks do all the work.
    loop {
        sleep(Duration::from_secs(3600)).await;
    }
}
