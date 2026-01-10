use crate::app::{AppEvent, ExecEvent};
use crate::app::state::OpenOrderInfo;
use anyhow::{anyhow, Context, Result};
use bigdecimal::BigDecimal;
use dydx::indexer::{IndexerClient, IndexerConfig, RestConfig, SockConfig};
use dydx::indexer::{Denom, Height, PerpetualMarket, Subaccount};
use dydx::node::{Address, ChainId, NodeClient, NodeConfig, OrderBuilder, OrderSide, PublicAccount, Wallet};
use dydx_proto::dydxprotocol::clob::order::TimeInForce;
use dydx_proto::dydxprotocol::clob::OrderBatch;
use rustls::crypto::ring;
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::str::FromStr;
use std::sync::mpsc::Sender;
use std::thread;
use tokio::runtime::Runtime;
use chrono::{DateTime, Utc};
use dydx::node::OrderId as ProtoOrderId;
use dydx::node::OrderGoodUntil;

const DEFAULT_MAINNET_GRPC: &str = "https://dydx-ops-grpc.kingnodes.com:443";
const DEFAULT_TESTNET_GRPC: &str = "https://test-dydx-grpc.kingnodes.com";
const DEFAULT_MAINNET_INDEXER_HTTP: &str = "https://indexer.dydx.trade";
const DEFAULT_MAINNET_INDEXER_WS: &str = "wss://indexer.dydx.trade/v4/ws";
const DEFAULT_TESTNET_INDEXER_HTTP: &str = "https://indexer.v4testnet.dydx.exchange";
const DEFAULT_TESTNET_INDEXER_WS: &str = "wss://indexer.v4testnet.dydx.exchange/v4/ws";
const DEFAULT_FEE_DENOM: &str =
    "ibc/8E27BA2D5493AF5636760E354E46004562C46AB7EC0CC4C1CA14E9E20E2545B5";

#[derive(Clone, Debug)]
pub struct OrderRequest {
    pub ticker: String,
    pub side: String,
    pub size: f64,
    pub leverage: f64,
    pub price_hint: f64,
    pub master_address: String,
    pub session_mnemonic: String,
    pub authenticator_id: u64,
    pub grpc_endpoint: String,
    pub chain_id: String,
    pub reduce_only: bool,
}

#[derive(Clone, Debug)]
pub struct CancelOrdersRequest {
    pub orders: Vec<OpenOrderInfo>,
    pub master_address: String,
    pub session_mnemonic: String,
    pub authenticator_id: u64,
    pub grpc_endpoint: String,
    pub chain_id: String,
}

pub fn spawn_real_order(tx: Sender<AppEvent>, req: OrderRequest) {
    thread::spawn(move || match place_order(req) {
        Ok(tx_hash) => {
            let _ = tx.send(AppEvent::Exec(ExecEvent::OrderSent { tx_hash }));
        }
        Err(err) => {
            let _ = tx.send(AppEvent::Exec(ExecEvent::OrderFailed {
                message: err.to_string(),
            }));
        }
    });
}

pub fn spawn_cancel_orders(tx: Sender<AppEvent>, req: CancelOrdersRequest) {
    thread::spawn(move || match cancel_orders(req) {
        Ok(message) => {
            let _ = tx.send(AppEvent::Exec(ExecEvent::OrderCancelStatus {
                ok: true,
                message,
            }));
        }
        Err(err) => {
            let _ = tx.send(AppEvent::Exec(ExecEvent::OrderCancelStatus {
                ok: false,
                message: err.to_string(),
            }));
        }
    });
}

fn place_order(req: OrderRequest) -> Result<String> {
    let _ = ring::default_provider().install_default();
    let rt = Runtime::new().context("init trade runtime")?;

    if !req.leverage.is_finite() || req.leverage <= 0.0 {
        return Err(anyhow!("leverage must be > 0"));
    }

    let chain_id = parse_chain_id(&req.chain_id)?;
    let endpoint = if req.grpc_endpoint.trim().is_empty() {
        default_grpc_endpoint(&chain_id)
    } else {
        req.grpc_endpoint.clone()
    };
    let denom = Denom::from_str(DEFAULT_FEE_DENOM)?;
    let config = NodeConfig {
        endpoint,
        timeout: 5_000,
        chain_id: chain_id.clone(),
        fee_denom: denom,
        manage_sequencing: true,
    };
    let mut client = rt.block_on(NodeClient::connect(config)).context("connect node")?;

    let indexer = IndexerClient::new(default_indexer_config(&chain_id));
    let wallet = Wallet::from_mnemonic(&req.session_mnemonic).context("session mnemonic")?;
    let mut account = rt
        .block_on(wallet.account(0, &mut client))
        .context("load session account")?;

    let master_address: Address = req
        .master_address
        .parse()
        .map_err(|_| anyhow!("invalid master address"))?;
    let master_public = rt
        .block_on(PublicAccount::updated(master_address.clone(), &mut client))
        .context("load master account")?;
    account
        .authenticators_mut()
        .add(master_public, req.authenticator_id);

    let ticker = req.ticker.clone();
    let market = rt
        .block_on(indexer.markets().get_perpetual_market(&ticker.clone().into()))
        .context("load market metadata")?;

    let side = parse_side(&req.side)?;
    let size = parse_quantity(req.size)?;
    let price = select_price(req.price_hint, &market, side)?;
    let height = rt
        .block_on(client.latest_block_height())
        .context("fetch latest height")?;
    let client_id = rand::random::<u32>();

    let subaccount = Subaccount {
        address: master_address,
        number: 0u32
            .try_into()
            .map_err(|_| anyhow!("invalid subaccount number"))?,
    };

    let (_id, order) = OrderBuilder::new(market, subaccount)
        .market(side, size)
        .reduce_only(req.reduce_only)
        .price(price)
        .time_in_force(TimeInForce::Unspecified)
        .until(height.ahead(10))
        .build(client_id)
        .context("build order")?;

    let tx_hash = rt
        .block_on(client.place_order(&mut account, order))
        .context("place order")?;
    Ok(tx_hash)
}

fn cancel_orders(req: CancelOrdersRequest) -> Result<String> {
    let _ = ring::default_provider().install_default();
    let rt = Runtime::new().context("init trade runtime")?;

    let chain_id = parse_chain_id(&req.chain_id)?;
    let endpoint = if req.grpc_endpoint.trim().is_empty() {
        default_grpc_endpoint(&chain_id)
    } else {
        req.grpc_endpoint.clone()
    };
    let denom = Denom::from_str(DEFAULT_FEE_DENOM)?;
    let config = NodeConfig {
        endpoint,
        timeout: 5_000,
        chain_id: chain_id.clone(),
        fee_denom: denom,
        manage_sequencing: true,
    };
    let mut client = rt.block_on(NodeClient::connect(config)).context("connect node")?;

    let wallet = Wallet::from_mnemonic(&req.session_mnemonic).context("session mnemonic")?;
    let mut account = rt
        .block_on(wallet.account(0, &mut client))
        .context("load session account")?;

    let master_address: Address = req
        .master_address
        .parse()
        .map_err(|_| anyhow!("invalid master address"))?;
    let master_public = rt
        .block_on(PublicAccount::updated(master_address.clone(), &mut client))
        .context("load master account")?;
    account
        .authenticators_mut()
        .add(master_public, req.authenticator_id);

    let subaccount = Subaccount {
        address: master_address,
        number: 0u32
            .try_into()
            .map_err(|_| anyhow!("invalid subaccount number"))?,
    };

    let mut short_term_batches: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut long_term_orders: Vec<OpenOrderInfo> = Vec::new();
    for order in &req.orders {
        if order.order_flags == 0 {
            short_term_batches
                .entry(order.clob_pair_id)
                .or_default()
                .push(order.client_id);
        } else {
            long_term_orders.push(order.clone());
        }
    }

    let mut tx_hashes: Vec<String> = Vec::new();
    let mut cancel_count = 0usize;
    if !short_term_batches.is_empty() {
        let mut batches: Vec<OrderBatch> = Vec::new();
        for (clob_pair_id, client_ids) in short_term_batches {
            if !client_ids.is_empty() {
                cancel_count += client_ids.len();
                batches.push(OrderBatch {
                    clob_pair_id,
                    client_ids,
                });
            }
        }
        let height = rt
            .block_on(client.latest_block_height())
            .context("fetch latest height")?;
        let tx_hash = rt
            .block_on(client.batch_cancel_orders(
                &mut account,
                subaccount.clone(),
                batches,
                height.ahead(10),
            ))
            .context("batch cancel orders")?;
        tx_hashes.push(tx_hash);
    }

    for order in long_term_orders {
        let until = if let Some(block) = order.good_til_block {
            OrderGoodUntil::Block(Height(block))
        } else if let Some(time_str) = &order.good_til_block_time {
            let Ok(parsed) = DateTime::parse_from_rfc3339(time_str) else {
                continue;
            };
            OrderGoodUntil::Time(parsed.with_timezone(&Utc))
        } else {
            continue;
        };
        let order_id = ProtoOrderId {
            subaccount_id: Some(subaccount.clone().into()),
            client_id: order.client_id,
            order_flags: order.order_flags,
            clob_pair_id: order.clob_pair_id,
        };
        let tx_hash = rt
            .block_on(client.cancel_order(&mut account, order_id, until))
            .context("cancel order")?;
        tx_hashes.push(tx_hash);
        cancel_count += 1;
    }

    if tx_hashes.is_empty() {
        return Ok("No cancelable orders.".to_string());
    }
    Ok(format!(
        "Cancel broadcast: {} order(s) in {} tx(s)",
        cancel_count,
        tx_hashes.len()
    ))
}

fn parse_chain_id(chain_id: &str) -> Result<ChainId> {
    if chain_id.eq_ignore_ascii_case("dydx-mainnet-1") || chain_id.eq_ignore_ascii_case("mainnet")
    {
        Ok(ChainId::Mainnet1)
    } else if chain_id.eq_ignore_ascii_case("dydx-testnet-4")
        || chain_id.eq_ignore_ascii_case("testnet")
    {
        Ok(ChainId::Testnet4)
    } else {
        Err(anyhow!("unsupported chain id: {}", chain_id))
    }
}

fn default_grpc_endpoint(chain_id: &ChainId) -> String {
    match chain_id {
        ChainId::Mainnet1 => DEFAULT_MAINNET_GRPC.to_string(),
        ChainId::Testnet4 => DEFAULT_TESTNET_GRPC.to_string(),
    }
}

fn default_indexer_config(chain_id: &ChainId) -> IndexerConfig {
    let (rest, sock) = match chain_id {
        ChainId::Mainnet1 => (DEFAULT_MAINNET_INDEXER_HTTP, DEFAULT_MAINNET_INDEXER_WS),
        ChainId::Testnet4 => (DEFAULT_TESTNET_INDEXER_HTTP, DEFAULT_TESTNET_INDEXER_WS),
    };
    IndexerConfig {
        rest: RestConfig {
            endpoint: rest.to_string(),
        },
        sock: SockConfig {
            endpoint: sock.to_string(),
            timeout: 1_000,
            rate_limit: NonZeroU32::new(2).unwrap(),
        },
    }
}

fn parse_side(side: &str) -> Result<OrderSide> {
    if side.eq_ignore_ascii_case("buy") {
        Ok(OrderSide::Buy)
    } else if side.eq_ignore_ascii_case("sell") {
        Ok(OrderSide::Sell)
    } else {
        Err(anyhow!("unsupported side: {}", side))
    }
}

fn parse_quantity(size: f64) -> Result<BigDecimal> {
    if !size.is_finite() || size <= 0.0 {
        return Err(anyhow!("size must be > 0"));
    }
    let raw = format!("{:.8}", size);
    BigDecimal::from_str(&raw).context("parse size")
}

fn select_price(price_hint: f64, market: &PerpetualMarket, side: OrderSide) -> Result<BigDecimal> {
    let base = if price_hint.is_finite() && price_hint > 0.0 {
        BigDecimal::from_str(&format!("{:.10}", price_hint)).context("parse price hint")?
    } else if let Some(oracle) = &market.oracle_price {
        oracle.0.clone()
    } else {
        return Err(anyhow!("missing price hint and oracle price"));
    };

    if base <= BigDecimal::from(0) {
        return Err(anyhow!("invalid price"));
    }

    let slippage = match side {
        OrderSide::Buy => "1.005",
        OrderSide::Sell => "0.995",
        OrderSide::Unspecified => "1.0",
    };
    let factor = BigDecimal::from_str(slippage).context("parse slippage")?;
    Ok(base * factor)
}
