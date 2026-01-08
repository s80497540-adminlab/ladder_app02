mod app;
mod debug_hooks;
mod exec;
mod feed;

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

    if ui.get_chart_enabled() && ui.get_history_valve_open() {
        let ticker = ui.get_current_ticker().to_string();
        let full = ui.get_render_all_candles();
        spawn_history_load(tx.clone(), ticker, full);
    }

    let feed_started = Rc::new(Cell::new(false));
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
        let close_scheduled = close_scheduled.clone();
        pump.start(TimerMode::Repeated, Duration::from_millis(16), move || {
            let frame_start = Instant::now();
            // Drain events quickly
            let mut any = false;
            let mut events = 0usize;
            while let Ok(ev) = rx.try_recv() {
                any = true;
                events += 1;
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
                runtime.borrow_mut().handle_event(ev);
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
