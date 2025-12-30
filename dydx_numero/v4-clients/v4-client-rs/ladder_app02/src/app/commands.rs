use super::event::{AppEvent, UiEvent};

pub fn wire_ui(ui: &crate::AppWindow, tx: std::sync::mpsc::Sender<AppEvent>) {
    // Simple helper
    let send = move |e: UiEvent, tx: &std::sync::mpsc::Sender<AppEvent>| {
        let _ = tx.send(AppEvent::Ui(e));
    };

    // --- Top controls ---
    {
        let tx = tx.clone();
        ui.on_ticker_changed(move |t| {
            let _ = tx.send(AppEvent::Ui(UiEvent::TickerChanged { ticker: t.to_string() }));
        });
    }
    {
        let tx = tx.clone();
        ui.on_mode_changed(move |m| {
            let _ = tx.send(AppEvent::Ui(UiEvent::ModeChanged { mode: m.to_string() }));
        });
    }
    {
        let tx = tx.clone();
        ui.on_time_mode_changed(move |tm| {
            let _ = tx.send(AppEvent::Ui(UiEvent::TimeModeChanged { time_mode: tm.to_string() }));
        });
    }
    {
        let tx = tx.clone();
        ui.on_candle_tf_changed(move |tf| {
            let _ = tx.send(AppEvent::Ui(UiEvent::CandleTfChanged { tf_secs: tf }));
        });
    }
    {
        let tx = tx.clone();
        ui.on_candle_window_changed(move |w| {
            let _ = tx.send(AppEvent::Ui(UiEvent::CandleWindowChanged { window_min: w }));
        });
    }
    {
        let tx = tx.clone();
        ui.on_dom_depth_changed(move |d| {
            let _ = tx.send(AppEvent::Ui(UiEvent::DomDepthChanged { depth: d }));
        });
    }

    // --- Actions ---
    {
        let tx = tx.clone();
        ui.on_send_order(move || {
            let _ = tx.send(AppEvent::Ui(UiEvent::SendOrder));
        });
    }
    {
        let tx = tx.clone();
        ui.on_reload_data(move || {
            let _ = tx.send(AppEvent::Ui(UiEvent::ReloadData));
        });
    }
    {
        let tx = tx.clone();
        ui.on_run_script(move || {
            let _ = tx.send(AppEvent::Ui(UiEvent::RunScript));
        });
    }
    {
        let tx = tx.clone();
        ui.on_deposit(move |amt| {
            let _ = tx.send(AppEvent::Ui(UiEvent::Deposit { amount: amt }));
        });
    }
    {
        let tx = tx.clone();
        ui.on_withdraw(move |amt| {
            let _ = tx.send(AppEvent::Ui(UiEvent::Withdraw { amount: amt }));
        });
    }

    // --- REAL gating controls ---
    {
        let tx = tx.clone();
        ui.on_trade_real_mode_toggled(move |enabled| {
            let _ = tx.send(AppEvent::Ui(UiEvent::TradeRealModeToggled { enabled }));
        });
    }
    {
        let tx = tx.clone();
        ui.on_arm_real_request(move |phrase| {
            let _ = tx.send(AppEvent::Ui(UiEvent::ArmRealRequest { phrase: phrase.to_string() }));
        });
    }
    {
        let tx = tx.clone();
        ui.on_disarm_real(move || {
            let _ = tx.send(AppEvent::Ui(UiEvent::DisarmReal));
        });
    }

    // (Optional) if you want to use the helper above:
    let _ = send;
}
