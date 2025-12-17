use anyhow::Result;
use std::env;

use dydx_client::config::ClientConfig;
use dydx_client::indexer::{Feeds, IndexerClient, SubaccountsMessage};
use dydx_client::node::{NodeClient, Wallet};

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

    // node just to derive subaccount safely + sync
    let mut node = NodeClient::connect(config.node).await?;
    let account = wallet.account(0, &mut node).await?;
    let sub = account.subaccount(0)?;

    let mut indexer = IndexerClient::new(config.indexer);
    let mut feeds: Feeds<'_> = indexer.feed();

    println!("listening to subaccount updates for {}#{}", sub.address, sub.number);

    let mut feed = feeds.subaccounts(sub, false).await?;

    while let Some(msg) = feed.recv().await {
        match msg {
            SubaccountsMessage::Initial(init) => {
                println!("INITIAL subaccount snapshot:\n{:#?}", init.contents);
            }
            SubaccountsMessage::Update(upd) => {
                println!("UPDATE:\n{:#?}", upd.contents);
            }
        }
    }

    Ok(())
}
