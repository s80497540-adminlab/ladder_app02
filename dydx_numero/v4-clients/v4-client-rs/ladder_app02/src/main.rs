mod app;
mod exec;
mod feed;

mod candle_agg;
mod settings;
mod signer;
mod persist;

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

    // Dummy feed so you can verify Phase-2 pipeline instantly
    feed::dummy::start_dummy_feed(tx.clone());

    // --- UI-thread event pump (drain channel, reduce, render) ---
    // This is the key: ALL UI touching happens on UI thread via this timer.
    let pump = Timer::default();
    {
        let runtime = runtime.clone();
        pump.start(TimerMode::Repeated, Duration::from_millis(16), move || {
            // Drain events quickly
            let mut any = false;
            while let Ok(ev) = rx.try_recv() {
                any = true;
                runtime.borrow_mut().handle_event(ev);
            }

            // If no external events arrived, still tick once per second for clock/arming TTL, etc.
            // We do this inside the runtime; it rate-limits itself.
            runtime.borrow_mut().tick_if_needed();

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
