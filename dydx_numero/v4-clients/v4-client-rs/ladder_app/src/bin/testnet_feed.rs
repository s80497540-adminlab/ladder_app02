use anyhow::Result;
use dydx_client::config::ClientConfig;
use dydx_client::indexer::{Feed, Feeds, IndexerClient, OrdersMessage, Ticker};

#[tokio::main]
async fn main() -> Result<()> {
    // async in your repo
    let config = ClientConfig::from_file("client/tests/testnet.toml").await?;

    // IndexerClient::new is sync
    let mut indexer = IndexerClient::new(config.indexer);

    // Feeds dispatcher (websocket)
    let mut feeds: Feeds<'_> = indexer.feed();

    let ticker = Ticker("ETH-USD".to_string());

    // ✅ correct: (&Ticker, batched_bool)
    let mut feed: Feed<OrdersMessage> = feeds.orders(&ticker, false).await?;

    println!("connected to dYdX testnet orderbook deltas for ETH-USD");

    while let Some(msg) = feed.recv().await {
        println!("orders msg: {msg:#?}");
    }

    Ok(())
}
