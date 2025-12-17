// ladder_app/src/bin/testnet_place_order_short.rs
//
// Minimal testnet market order placement.
// Run:
//   export DYDX_TESTNET_MNEMONIC='mirror actor ... wait'
//   cargo run -p ladder_app --bin testnet_place_order_short

use anyhow::Result;
use bigdecimal::BigDecimal;
use std::env;
use std::str::FromStr;

use dydx_client::config::ClientConfig;
use dydx_client::indexer::IndexerClient;
use dydx_client::node::{NodeClient, OrderBuilder, OrderSide, Wallet};
use dydx_proto::dydxprotocol::clob::order::TimeInForce;

const ETH_USD_TICKER: &str = "ETH-USD";

fn init_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

#[tokio::main]
async fn main() -> Result<()> {
    init_crypto_provider();

    let config = ClientConfig::from_file("client/tests/testnet.toml").await?;

    let raw = env::var("DYDX_TESTNET_MNEMONIC")
        .expect("set DYDX_TESTNET_MNEMONIC in your shell");
    let mnemonic = raw.split_whitespace().collect::<Vec<_>>().join(" ");

    let wallet = Wallet::from_mnemonic(&mnemonic)?;

    let mut client = NodeClient::connect(config.node).await?;
    let indexer = IndexerClient::new(config.indexer);

    let mut account = wallet.account(0, &mut client).await?;
    let subaccount = account.subaccount(0)?;

    let market = indexer
        .markets()
        .get_perpetual_market(&ETH_USD_TICKER.into())
        .await?;

    let current_block_height = client.latest_block_height().await?;

    let size = BigDecimal::from_str("0.02")?;

    let (_id, order) = OrderBuilder::new(market, subaccount)
        .market(OrderSide::Buy, size)
        .reduce_only(false)
        .price(100) // slippage protection
        .time_in_force(TimeInForce::Unspecified)
        .until(current_block_height.ahead(10))
        .build(123456)?;

    let tx_hash = client.place_order(&mut account, order).await?;
    println!("Broadcast tx hash: {tx_hash:?}");

    Ok(())
}
