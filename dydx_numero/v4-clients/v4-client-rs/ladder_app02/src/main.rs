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
use slint::{Timer, TimerMode};
use std::{cell::RefCell, rc::Rc, time::Duration};

slint::include_modules!();

fn main() -> Result<()> {
    // --- UI ---
    let ui = AppWindow::new()?;

    // --- Persistence (load -> apply; then autosave runs in background) ---
    let persistence = persist::Persistence::new()?;
    println!("CONFIG_PATH = {:?}", persistence.config_path());

    let cfg = persistence.load();
    persist::Persistence::apply_to_ui(&cfg, &ui);
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

    // Live feed: tail persisted daemon output so data collected 24/7 is available immediately.
    feed::daemon::start_daemon_bridge(tx.clone());

    // Dummy feed fallback so the UI stays functional if the daemon is not running yet.
    let has_live_cache =
        feed_shared::snapshot_path().exists() || feed_shared::event_log_path().exists();
    if !has_live_cache {
        feed::dummy::start_dummy_feed(tx.clone());
    }

    // --- UI-thread event pump (drain channel, reduce, render) ---
    // This is the key: ALL UI touching happens on UI thread via this timer.
    let pump = Timer::default();
    {
        let runtime = runtime.clone();
        let history_tx = tx.clone();
        pump.start(TimerMode::Repeated, Duration::from_millis(16), move || {
            // Drain events quickly
            let mut any = false;
            while let Ok(ev) = rx.try_recv() {
                any = true;
                if let app::AppEvent::Ui(app::UiEvent::TickerChanged { ticker }) = &ev {
                    let rt = runtime.borrow();
                    if rt.state.history_valve_open {
                        let full = rt.state.render_all_candles;
                        spawn_history_load(history_tx.clone(), ticker.clone(), full);
                    }
                }
                if let app::AppEvent::Ui(app::UiEvent::RenderModeChanged { full }) = &ev {
                    let rt = runtime.borrow();
                    if rt.state.history_valve_open {
                        let ticker = rt.state.current_ticker.clone();
                        spawn_history_load(history_tx.clone(), ticker, *full);
                    }
                }
                if let app::AppEvent::Ui(app::UiEvent::HistoryValveChanged { open }) = &ev {
                    if *open {
                        let rt = runtime.borrow();
                        let ticker = rt.state.current_ticker.clone();
                        let full = rt.state.render_all_candles;
                        spawn_history_load(history_tx.clone(), ticker, full);
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
        });
    }

    ui.run()?;
    Ok(())
}

fn spawn_history_load(tx: std::sync::mpsc::Sender<app::AppEvent>, ticker: String, full: bool) {
    std::thread::spawn(move || {
        let ticks = app::AppState::read_mid_ticks_for_ticker(&ticker, full);
        let _ = tx.send(app::AppEvent::HistoryLoaded { ticker, ticks, full });
    });
}
