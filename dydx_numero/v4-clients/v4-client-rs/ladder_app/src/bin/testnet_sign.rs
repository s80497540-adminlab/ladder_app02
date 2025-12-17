use anyhow::Result;
use std::env;

use dydx_client::config::ClientConfig;
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

    let mut client = NodeClient::connect(config.node).await?;
    let account0 = wallet.account(0, &mut client).await?;

    println!("Testnet address (account 0): {}", account0.address());
    println!(
        "sequence={} account_number={}",
        account0.sequence_number(),
        account0.account_number()
    );

    let sub0 = account0.subaccount(0)?;
    println!(
        "subaccount 0 => address {} number {} (parent? {})",
        sub0.address,
        sub0.number,
        sub0.is_parent()
    );

    Ok(())
}
