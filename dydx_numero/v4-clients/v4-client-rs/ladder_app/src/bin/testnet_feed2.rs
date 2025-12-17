use anyhow::Result;
use bigdecimal::Zero;
use dydx_client::config::ClientConfig;
use dydx_client::indexer::{Feed, Feeds, IndexerClient, OrdersMessage, Ticker};

use std::collections::BTreeMap;
use dydx_client::indexer::{OrderbookResponsePriceLevel, Price, Quantity};

#[derive(Default, Clone, Debug)]
pub struct LiveBook {
    pub bids: BTreeMap<Price, Quantity>,
    pub asks: BTreeMap<Price, Quantity>,
}

impl LiveBook {
    pub fn apply_levels(map: &mut BTreeMap<Price, Quantity>, levels: Vec<OrderbookResponsePriceLevel>) {
        for lvl in levels {
            let p = lvl.price;
            let s = lvl.size;
            if s.0.is_zero() {
                map.remove(&p);
            } else {
                map.insert(p, s);
            }
        }
    }

    pub fn apply_initial(&mut self, bids: Vec<OrderbookResponsePriceLevel>, asks: Vec<OrderbookResponsePriceLevel>) {
        self.bids.clear();
        self.asks.clear();
        Self::apply_levels(&mut self.bids, bids);
        Self::apply_levels(&mut self.asks, asks);
    }

    pub fn apply_update(
        &mut self,
        bids: Option<Vec<OrderbookResponsePriceLevel>>,
        asks: Option<Vec<OrderbookResponsePriceLevel>>,
    ) {
        if let Some(b) = bids {
            Self::apply_levels(&mut self.bids, b);
        }
        if let Some(a) = asks {
            Self::apply_levels(&mut self.asks, a);
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let config = ClientConfig::from_file("client/tests/testnet.toml").await?;
    let mut indexer = IndexerClient::new(config.indexer);
    let mut feeds: Feeds<'_> = indexer.feed();

    let ticker = Ticker("ETH-USD".to_string());
    let mut feed: Feed<OrdersMessage> = feeds.orders(&ticker, false).await?;

    println!("connected to testnet deltas");

    let mut book = LiveBook::default();

    while let Some(msg) = feed.recv().await {
        match msg {
            OrdersMessage::Initial(init) => {
                let bids = init.contents.bids;
                let asks = init.contents.asks;
                book.apply_initial(bids, asks);

                println!("snapshot loaded. bids={} asks={}", book.bids.len(), book.asks.len());
            }
            OrdersMessage::Update(upd) => {
                let bids = upd.contents.bids;
                let asks = upd.contents.asks;
                book.apply_update(bids, asks);

                // show top-of-book for proof
                let best_bid = book.bids.iter().next_back().map(|(p, s)| (p.clone(), s.clone()));
                let best_ask = book.asks.iter().next().map(|(p, s)| (p.clone(), s.clone()));
                println!("best bid={best_bid:?} best ask={best_ask:?}");
            }
        }
    }

    Ok(())
}
