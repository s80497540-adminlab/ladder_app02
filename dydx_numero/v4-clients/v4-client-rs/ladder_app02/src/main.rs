mod app;
mod debug_hooks;
mod exec;
mod feed;
mod keplr_bridge;
mod trade_engine;

mod candle_agg;
mod persist;
mod settings;
mod signer;
use ladder_app02::feed_shared;

use anyhow::Result;
use serde_json::Value;
use slint::{Timer, TimerMode};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

slint::include_modules!();

fn main() -> Result<()> {
    // --- UI ---
    let ui = AppWindow::new()?;

    // --- Persistence (load -> apply; then autosave runs in background) ---
    let persistence = persist::Persistence::new()?;
    println!("CONFIG_PATH = {:?}", persistence.config_path());

    let cfg = persistence.load();
    persist::Persistence::apply_to_ui(&cfg, &ui);
    ui.set_ticker_input(ui.get_current_ticker());
    persistence.start_autosave(ui.as_weak())?;

    // --- Event bus ---
    let (tx, rx) = std::sync::mpsc::channel::<app::AppEvent>();

    // Wire UI callbacks -> AppEvent::Ui(...)
    app::commands::wire_ui(&ui, tx.clone());

    // Runtime holder (state + reducer + render)
    let runtime = Rc::new(RefCell::new(app::AppRuntime::new(ui.as_weak())));

    // Seed state from UI (after persistence apply)
    {
        let mut rt = runtime.borrow_mut();
        rt.state = app::AppState::from_ui(&ui);
        rt.render(); // initial paint
    }
    {
        let rt = runtime.borrow();
        start_market_poll(
            tx.clone(),
            rt.state.market_poll_interval.clone(),
            rt.state.market_poll_ticker.clone(),
        );
    }
    let account_poll_cfg = Arc::new(Mutex::new(AccountPollConfig::default()));
    start_account_poll(tx.clone(), account_poll_cfg.clone());
    {
        let rt = runtime.borrow();
        sync_account_poll_config(&account_poll_cfg, &rt.state);
    }

    if ui.get_chart_enabled() && ui.get_history_valve_open() {
        let ticker = ui.get_current_ticker().to_string();
        let full = ui.get_render_all_candles();
        spawn_history_load(tx.clone(), ticker, full);
    }

    let feed_started = Rc::new(Cell::new(false));
    let keplr_running = Rc::new(Cell::new(false));
    let close_scheduled = Rc::new(Cell::new(false));
    if ui.get_feed_enabled() {
        start_feeds(tx.clone());
        feed_started.set(true);
    }

    // --- UI-thread event pump (drain channel, reduce, render) ---
    // This is the key: ALL UI touching happens on UI thread via this timer.
    let pump = Timer::default();
    {
        let runtime = runtime.clone();
        let history_tx = tx.clone();
        let feed_tx = tx.clone();
        let feed_started = feed_started.clone();
        let keplr_running = keplr_running.clone();
        let close_scheduled = close_scheduled.clone();
        pump.start(TimerMode::Repeated, Duration::from_millis(16), move || {
            let frame_start = Instant::now();
            // Drain events quickly
            let mut any = false;
            let mut events = 0usize;
            while let Ok(ev) = rx.try_recv() {
                any = true;
                events += 1;
                if matches!(
                    ev,
                    app::AppEvent::Ui(app::UiEvent::SettingsConnectWallet)
                        | app::AppEvent::Ui(app::UiEvent::SettingsCreateSession { .. })
                ) {
                    let rt = runtime.borrow();
                    if !rt.state.settings_auto_sign {
                        let _ = feed_tx.send(app::AppEvent::Exec(app::ExecEvent::KeplrSessionFailed {
                            message: "Enable Auto-sign first.".to_string(),
                        }));
                    } else if !keplr_running.get() {
                        let ttl = rt
                            .state
                            .settings_session_ttl_minutes
                            .trim()
                            .parse::<u64>()
                            .unwrap_or(30);
                        let chain_id = if rt.state.settings_network.eq_ignore_ascii_case("mainnet")
                        {
                            "dydx-mainnet-1".to_string()
                        } else {
                            "dydx-testnet-4".to_string()
                        };
                        let cfg = keplr_bridge::KeplrBridgeConfig {
                            chain_id,
                            grpc_endpoint: rt.state.settings_rpc_endpoint.clone(),
                            fee_denom: String::new(),
                            session_ttl_minutes: ttl,
                        };
                        if keplr_bridge::start_keplr_bridge(feed_tx.clone(), cfg).is_ok() {
                            keplr_running.set(true);
                        }
                    }
                }
                if let app::AppEvent::Ui(app::UiEvent::SendOrder) = &ev {
                    let rt = runtime.borrow();
                    if rt.state.trade_real_mode && rt.state.trade_real_armed {
                        let now = app::state::now_unix();
                        let session_ok = rt
                            .state
                            .session_expires_at_unix
                            .map(|exp| now < exp)
                            .unwrap_or(false);
                        if session_ok
                            && rt.state.session_authenticator_id.is_some()
                            && !rt.state.session_mnemonic.is_empty()
                            && !rt.state.session_master_address.is_empty()
                        {
                            trade_engine::spawn_real_order(
                                feed_tx.clone(),
                                trade_engine::OrderRequest {
                                    ticker: rt.state.current_ticker.clone(),
                                    side: rt.state.trade_side.clone(),
                                    size: rt.state.trade_size as f64,
                                    leverage: rt.state.trade_leverage as f64,
                                    price_hint: rt.state.metrics.mid,
                                    master_address: rt.state.session_master_address.clone(),
                                    session_mnemonic: rt.state.session_mnemonic.clone(),
                                    authenticator_id: rt.state.session_authenticator_id.unwrap(),
                                    grpc_endpoint: rt.state.settings_rpc_endpoint.clone(),
                                    chain_id: rt.state.settings_network.clone(),
                                    reduce_only: false,
                                },
                            );
                        } else {
                            let _ = feed_tx.send(app::AppEvent::Exec(app::ExecEvent::OrderFailed {
                                message: "Session inactive or missing.".to_string(),
                            }));
                        }
                    }
                }
                if let app::AppEvent::Ui(app::UiEvent::ClosePositionRequested) = &ev {
                    let rt = runtime.borrow();
                    if rt.state.position_size > 0.0
                        && !rt.state.position_side.eq_ignore_ascii_case("flat")
                        && rt.state.trade_real_mode
                        && rt.state.trade_real_armed
                    {
                        let now = app::state::now_unix();
                        let session_ok = rt
                            .state
                            .session_expires_at_unix
                            .map(|exp| now < exp)
                            .unwrap_or(false);
                        if session_ok
                            && rt.state.session_authenticator_id.is_some()
                            && !rt.state.session_mnemonic.is_empty()
                            && !rt.state.session_master_address.is_empty()
                        {
                            let close_side = if rt.state.position_side.eq_ignore_ascii_case("long")
                            {
                                "Sell".to_string()
                            } else {
                                "Buy".to_string()
                            };
                            trade_engine::spawn_real_order(
                                feed_tx.clone(),
                                trade_engine::OrderRequest {
                                    ticker: rt.state.current_ticker.clone(),
                                    side: close_side,
                                    size: rt.state.position_size as f64,
                                    leverage: rt.state.trade_leverage as f64,
                                    price_hint: rt.state.metrics.mid,
                                    master_address: rt.state.session_master_address.clone(),
                                    session_mnemonic: rt.state.session_mnemonic.clone(),
                                    authenticator_id: rt.state.session_authenticator_id.unwrap(),
                                    grpc_endpoint: rt.state.settings_rpc_endpoint.clone(),
                                    chain_id: rt.state.settings_network.clone(),
                                    reduce_only: true,
                                },
                            );
                        } else {
                            let _ = feed_tx.send(app::AppEvent::Exec(app::ExecEvent::OrderFailed {
                                message: "Session inactive or missing.".to_string(),
                            }));
                        }
                    }
                }
                if let app::AppEvent::Ui(app::UiEvent::CancelOpenOrdersRequested) = &ev {
                    let rt = runtime.borrow();
                    let now = app::state::now_unix();
                    let session_ok = rt
                        .state
                        .session_expires_at_unix
                        .map(|exp| now < exp)
                        .unwrap_or(false);
                    if rt.state.trade_real_mode
                        && rt.state.trade_real_armed
                        && session_ok
                        && rt.state.session_authenticator_id.is_some()
                        && !rt.state.session_mnemonic.is_empty()
                        && !rt.state.session_master_address.is_empty()
                    {
                        let ticker = rt.state.current_ticker.clone();
                        let orders: Vec<app::state::OpenOrderInfo> = rt
                            .state
                            .open_orders
                            .iter()
                            .filter(|ord| ord.ticker.eq_ignore_ascii_case(&ticker))
                            .cloned()
                            .collect();
                        if !orders.is_empty() {
                            trade_engine::spawn_cancel_orders(
                                feed_tx.clone(),
                                trade_engine::CancelOrdersRequest {
                                    orders,
                                    master_address: rt.state.session_master_address.clone(),
                                    session_mnemonic: rt.state.session_mnemonic.clone(),
                                    authenticator_id: rt.state.session_authenticator_id.unwrap(),
                                    grpc_endpoint: rt.state.settings_rpc_endpoint.clone(),
                                    chain_id: rt.state.settings_network.clone(),
                                },
                            );
                        } else {
                            let _ = feed_tx.send(app::AppEvent::Exec(
                                app::ExecEvent::OrderCancelStatus {
                                    ok: false,
                                    message: "No open orders for ticker.".to_string(),
                                },
                            ));
                        }
                    } else {
                        let _ = feed_tx.send(app::AppEvent::Exec(
                            app::ExecEvent::OrderCancelStatus {
                                ok: false,
                                message: "Cancel blocked (REAL/ARM/session)".to_string(),
                            },
                        ));
                    }
                }
                if let app::AppEvent::Ui(app::UiEvent::TickerChanged { ticker }) = &ev {
                    let rt = runtime.borrow();
                    if rt.state.history_valve_open && rt.state.chart_enabled {
                        let full = rt.state.render_all_candles;
                        spawn_history_load(history_tx.clone(), ticker.clone(), full);
                    }
                }
                if let app::AppEvent::Ui(app::UiEvent::RenderModeChanged { full }) = &ev {
                    let rt = runtime.borrow();
                    if rt.state.history_valve_open && rt.state.chart_enabled {
                        let ticker = rt.state.current_ticker.clone();
                        spawn_history_load(history_tx.clone(), ticker, *full);
                    }
                }
                if let app::AppEvent::Ui(app::UiEvent::ChartEnabledChanged { enabled }) = &ev {
                    if *enabled {
                        let rt = runtime.borrow();
                        if rt.state.history_valve_open {
                            let ticker = rt.state.current_ticker.clone();
                            let full = rt.state.render_all_candles;
                            spawn_history_load(history_tx.clone(), ticker, full);
                        }
                    }
                }
                if let app::AppEvent::Ui(app::UiEvent::HistoryValveChanged { open }) = &ev {
                    if *open {
                        let rt = runtime.borrow();
                        if rt.state.chart_enabled {
                            let ticker = rt.state.current_ticker.clone();
                            let full = rt.state.render_all_candles;
                            spawn_history_load(history_tx.clone(), ticker, full);
                        }
                    }
                }
                if let app::AppEvent::Ui(app::UiEvent::FeedEnabledChanged { enabled }) = &ev {
                    if *enabled && !feed_started.get() {
                        start_feeds(feed_tx.clone());
                        feed_started.set(true);
                    }
                }
                if let app::AppEvent::Exec(
                    app::ExecEvent::KeplrSessionCreated { .. }
                    | app::ExecEvent::KeplrSessionFailed { .. },
                ) = &ev
                {
                    keplr_running.set(false);
                }
                runtime.borrow_mut().handle_event(ev);
                {
                    let rt = runtime.borrow();
                    sync_account_poll_config(&account_poll_cfg, &rt.state);
                }
            }

            // If no external events arrived, still tick once per second for clock/arming TTL, etc.
            // We do this inside the runtime; it rate-limits itself.
            runtime.borrow_mut().tick_if_needed();
            runtime.borrow_mut().process_pending_history();

            if any {
                runtime.borrow_mut().render_if_dirty();
            } else {
                runtime.borrow_mut().render_if_dirty();
            }
            let frame_ms = frame_start.elapsed().as_secs_f32() * 1000.0;
            runtime.borrow_mut().update_perf(frame_ms, events);

            if runtime.borrow().state.close_after_save && !close_scheduled.get() {
                close_scheduled.set(true);
                let _ = slint::Timer::single_shot(Duration::from_millis(800), move || {
                    slint::quit_event_loop();
                });
            }
        });
    }

    ui.run()?;
    Ok(())
}

fn start_market_poll(
    tx: std::sync::mpsc::Sender<app::AppEvent>,
    poll_interval: Arc<AtomicU64>,
    poll_ticker: Arc<Mutex<String>>,
) {
    const MARKET_URL: &str = "https://indexer.dydx.trade/v4/perpetualMarkets";

    std::thread::spawn(move || {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build();
        let Ok(client) = client else {
            return;
        };
        loop {
            if let Ok(resp) = client.get(MARKET_URL).send() {
                if let Ok(json) = resp.json::<Value>() {
                    if let Some(markets) = json.get("markets").and_then(|v| v.as_object()) {
                        let mut markets_list: Vec<app::MarketInfo> =
                            Vec::with_capacity(markets.len());
                        for (ticker, meta) in markets {
                            let active = meta
                                .get("status")
                                .and_then(|v| v.as_str())
                                .map(|s| s.eq_ignore_ascii_case("ACTIVE"))
                                .unwrap_or(true);
                            markets_list.push(app::MarketInfo {
                                ticker: ticker.to_string(),
                                active,
                            });
                        }
                        if !markets_list.is_empty() {
                            let _ = tx.send(app::AppEvent::Feed(app::FeedEvent::MarketList {
                                markets: markets_list,
                            }));
                        }
                        let now = app::state::now_unix();
                        let ticker = poll_ticker
                            .lock()
                            .ok()
                            .map(|t| t.clone())
                            .unwrap_or_else(|| "BTC-USD".to_string());
                        if let Some(mkt) = markets.get(&ticker) {
                            let oracle_raw = mkt
                                .get("oraclePrice")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            if !oracle_raw.is_empty() {
                                let oracle_price = oracle_raw.parse::<f64>().unwrap_or(0.0);
                                let mark_raw = mkt
                                    .get("markPrice")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or(&oracle_raw)
                                    .to_string();
                                let mark_price = mark_raw.parse::<f64>().unwrap_or(0.0);
                                let _ = tx.send(app::AppEvent::Feed(app::FeedEvent::MarketPrice {
                                    ts_unix: now,
                                    ticker: ticker.to_string(),
                                    mark_price,
                                    mark_price_raw: mark_raw,
                                    oracle_price,
                                    oracle_price_raw: oracle_raw,
                                }));
                            }
                        }
                    }
                }
            }
            let secs = poll_interval.load(Ordering::Relaxed).max(1);
            std::thread::sleep(Duration::from_secs(secs));
        }
    });
}

#[derive(Clone, Default)]
struct AccountPollConfig {
    address: String,
    network: String,
    ticker: String,
}

fn start_account_poll(
    tx: std::sync::mpsc::Sender<app::AppEvent>,
    cfg: Arc<Mutex<AccountPollConfig>>,
) {
    const MAINNET_INDEXER_HTTP: &str = "https://indexer.dydx.trade";
    const TESTNET_INDEXER_HTTP: &str = "https://indexer.v4testnet.dydx.exchange";

    std::thread::spawn(move || {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(8))
            .build();
        let Ok(client) = client else {
            eprintln!("[account] http client init failed");
            return;
        };

        loop {
            let (address, network, ticker) = {
                let guard = cfg.lock().unwrap_or_else(|e| e.into_inner());
                (
                    guard.address.clone(),
                    guard.network.clone(),
                    guard.ticker.clone(),
                )
            };

            if address.trim().is_empty() {
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }

            let rest = if network.eq_ignore_ascii_case("testnet") {
                TESTNET_INDEXER_HTTP
            } else {
                MAINNET_INDEXER_HTTP
            };

            let account_url = format!("{rest}/v4/addresses/{address}/subaccountNumber/0");
            let resp = client.get(&account_url).send();
            let Ok(resp) = resp else {
                let _ = tx.send(app::AppEvent::Exec(
                    app::ExecEvent::AccountSnapshotError {
                        message: "account request failed".to_string(),
                    },
                ));
                std::thread::sleep(Duration::from_secs(5));
                continue;
            };

            if !resp.status().is_success() {
                let _ = tx.send(app::AppEvent::Exec(
                    app::ExecEvent::AccountSnapshotError {
                        message: format!("account http {}", resp.status()),
                    },
                ));
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }

            let json = resp.json::<Value>();
            let Ok(json) = json else {
                let _ = tx.send(app::AppEvent::Exec(
                    app::ExecEvent::AccountSnapshotError {
                        message: "account decode failed".to_string(),
                    },
                ));
                std::thread::sleep(Duration::from_secs(5));
                continue;
            };

            let sub = match json.get("subaccount") {
                Some(val) => val,
                None => {
                    let _ = tx.send(app::AppEvent::Exec(
                        app::ExecEvent::AccountSnapshotError {
                            message: "missing subaccount".to_string(),
                        },
                    ));
                    std::thread::sleep(Duration::from_secs(5));
                    continue;
                }
            };

            let equity = parse_json_number(sub.get("equity")).unwrap_or(0.0);
            let free = parse_json_number(sub.get("freeCollateral")).unwrap_or(0.0);
            let margin_enabled = sub
                .get("marginEnabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);

            let _ = tx.send(app::AppEvent::Exec(app::ExecEvent::AccountSnapshot {
                equity,
                free_collateral: free,
                margin_enabled,
            }));

            if !ticker.trim().is_empty() {
                let pos_url = format!(
                    "{rest}/v4/perpetualPositions?address={address}&subaccountNumber=0"
                );
                let pos_resp = client.get(&pos_url).send();
                let Ok(pos_resp) = pos_resp else {
                    let _ = tx.send(app::AppEvent::Exec(
                        app::ExecEvent::PositionSnapshotError {
                            message: "positions request failed".to_string(),
                        },
                    ));
                    std::thread::sleep(Duration::from_secs(5));
                    continue;
                };
                if !pos_resp.status().is_success() {
                    let _ = tx.send(app::AppEvent::Exec(
                        app::ExecEvent::PositionSnapshotError {
                            message: format!("positions http {}", pos_resp.status()),
                        },
                    ));
                    std::thread::sleep(Duration::from_secs(5));
                    continue;
                }
                let pos_json = pos_resp.json::<Value>();
                let Ok(pos_json) = pos_json else {
                    let _ = tx.send(app::AppEvent::Exec(
                        app::ExecEvent::PositionSnapshotError {
                            message: "positions decode failed".to_string(),
                        },
                    ));
                    std::thread::sleep(Duration::from_secs(5));
                    continue;
                };
                let mut found = false;
                if let Some(items) = pos_json.get("positions").and_then(|v| v.as_array()) {
                    for pos in items {
                        let market = pos.get("market").and_then(|v| v.as_str()).unwrap_or("");
                        if !market.eq_ignore_ascii_case(ticker.trim()) {
                            continue;
                        }
                        let side_raw = pos.get("side").and_then(|v| v.as_str()).unwrap_or("");
                        let side = match side_raw.to_ascii_lowercase().as_str() {
                            "long" => "Long",
                            "short" => "Short",
                            _ => "Flat",
                        };
                        let size = parse_json_number(pos.get("size")).unwrap_or(0.0);
                        let entry = parse_json_number(pos.get("entryPrice")).unwrap_or(0.0);
                        let pnl = parse_json_number(pos.get("unrealizedPnl")).unwrap_or(0.0);
                        let status = pos
                            .get("status")
                            .and_then(|v| v.as_str())
                            .unwrap_or("UNKNOWN");
                        let _ = tx.send(app::AppEvent::Exec(app::ExecEvent::PositionSnapshot {
                            ticker: market.to_string(),
                            side: side.to_string(),
                            size,
                            entry_price: entry,
                            unrealized_pnl: pnl,
                            status: status.to_string(),
                        }));
                        found = true;
                        break;
                    }
                }
                if !found {
                    let _ = tx.send(app::AppEvent::Exec(app::ExecEvent::PositionSnapshot {
                        ticker: ticker.clone(),
                        side: "Flat".to_string(),
                        size: 0.0,
                        entry_price: 0.0,
                        unrealized_pnl: 0.0,
                        status: "FLAT".to_string(),
                    }));
                }
            }

            let orders_url = format!(
                "{rest}/v4/orders?address={address}&subaccountNumber=0&status=OPEN&limit=200"
            );
            let orders_resp = client.get(&orders_url).send();
            let Ok(orders_resp) = orders_resp else {
                let _ = tx.send(app::AppEvent::Exec(app::ExecEvent::OpenOrdersError {
                    message: "orders request failed".to_string(),
                }));
                std::thread::sleep(Duration::from_secs(5));
                continue;
            };
            if !orders_resp.status().is_success() {
                let _ = tx.send(app::AppEvent::Exec(app::ExecEvent::OpenOrdersError {
                    message: format!("orders http {}", orders_resp.status()),
                }));
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }
            let orders_json = orders_resp.json::<Value>();
            let Ok(orders_json) = orders_json else {
                let _ = tx.send(app::AppEvent::Exec(app::ExecEvent::OpenOrdersError {
                    message: "orders decode failed".to_string(),
                }));
                std::thread::sleep(Duration::from_secs(5));
                continue;
            };
            let mut open_orders: Vec<app::OpenOrderInfo> = Vec::new();
            if let Some(items) = orders_json.get("orders").and_then(|v| v.as_array()) {
                for order in items {
                    let ticker_raw = order.get("ticker").and_then(|v| v.as_str()).unwrap_or("");
                    let client_id = parse_json_u32(order.get("clientId"));
                    let clob_pair_id = parse_json_u32(order.get("clobPairId"));
                    let order_flags = parse_json_u32(order.get("orderFlags"));
                    if client_id.is_none() || clob_pair_id.is_none() || order_flags.is_none() {
                        continue;
                    }
                    let good_til_block = parse_json_u32(order.get("goodTilBlock"));
                    let good_til_block_time = order
                        .get("goodTilBlockTime")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    open_orders.push(app::OpenOrderInfo {
                        ticker: ticker_raw.to_string(),
                        client_id: client_id.unwrap_or(0),
                        clob_pair_id: clob_pair_id.unwrap_or(0),
                        order_flags: order_flags.unwrap_or(0),
                        good_til_block,
                        good_til_block_time,
                    });
                }
            }
            let total = open_orders.len();
            let ticker_count = open_orders
                .iter()
                .filter(|ord| ord.ticker.eq_ignore_ascii_case(&ticker))
                .count();
            let _ = tx.send(app::AppEvent::Exec(app::ExecEvent::OpenOrdersSnapshot {
                total,
                ticker: ticker.clone(),
                ticker_count,
                orders: open_orders,
            }));

            std::thread::sleep(Duration::from_secs(5));
        }
    });
}

fn parse_json_number(value: Option<&Value>) -> Option<f64> {
    let value = value?;
    if let Some(num) = value.as_f64() {
        return Some(num);
    }
    if let Some(text) = value.as_str() {
        return text.trim().parse::<f64>().ok();
    }
    None
}

fn parse_json_u32(value: Option<&Value>) -> Option<u32> {
    let value = value?;
    if let Some(num) = value.as_u64() {
        return u32::try_from(num).ok();
    }
    if let Some(text) = value.as_str() {
        return text.trim().parse::<u32>().ok();
    }
    None
}

fn sync_account_poll_config(cfg: &Arc<Mutex<AccountPollConfig>>, state: &app::AppState) {
    let address = if !state.settings_wallet_address.trim().is_empty() {
        state.settings_wallet_address.clone()
    } else {
        state.session_master_address.clone()
    };
    let network = if state.settings_network.trim().is_empty() {
        "Mainnet".to_string()
    } else {
        state.settings_network.clone()
    };
    let ticker = state.current_ticker.clone();
    let mut guard = cfg.lock().unwrap_or_else(|e| e.into_inner());
    guard.address = address;
    guard.network = network;
    guard.ticker = ticker;
}

fn spawn_history_load(tx: std::sync::mpsc::Sender<app::AppEvent>, ticker: String, full: bool) {
    std::thread::spawn(move || {
        let ticks = app::AppState::read_mid_ticks_for_ticker(&ticker, full);
        let _ = tx.send(app::AppEvent::HistoryLoaded { ticker, ticks, full });
    });
}

fn start_feeds(tx: std::sync::mpsc::Sender<app::AppEvent>) {
    // Live feed: tail persisted daemon output so data collected 24/7 is available immediately.
    feed::daemon::start_daemon_bridge(tx.clone(), true);

    // Dummy feed fallback so the UI stays functional if the daemon is not running yet.
    let has_live_cache =
        feed_shared::snapshot_path().exists() || feed_shared::event_log_path().exists();
    if !has_live_cache {
        feed::dummy::start_dummy_feed(tx);
    }
}
