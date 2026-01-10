use super::event::{AppEvent, UiEvent};
use slint::{ComponentHandle, PhysicalSize};

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
        ui.on_feed_enabled_changed(move |enabled| {
            let _ = tx.send(AppEvent::Ui(UiEvent::FeedEnabledChanged { enabled }));
        });
    }
    {
        let tx = tx.clone();
        ui.on_chart_enabled_changed(move |enabled| {
            let _ = tx.send(AppEvent::Ui(UiEvent::ChartEnabledChanged { enabled }));
        });
    }
    {
        let tx = tx.clone();
        ui.on_depth_panel_changed(move |enabled| {
            let _ = tx.send(AppEvent::Ui(UiEvent::DepthPanelToggled { enabled }));
        });
    }
    {
        let tx = tx.clone();
        ui.on_trades_panel_changed(move |enabled| {
            let _ = tx.send(AppEvent::Ui(UiEvent::TradesPanelToggled { enabled }));
        });
    }
    {
        let tx = tx.clone();
        ui.on_volume_panel_changed(move |enabled| {
            let _ = tx.send(AppEvent::Ui(UiEvent::VolumePanelToggled { enabled }));
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
        ui.on_candle_price_mode_changed(move |mode| {
            let _ = tx.send(AppEvent::Ui(UiEvent::CandlePriceModeChanged {
                mode: mode.to_string(),
            }));
        });
    }
    {
        let tx = tx.clone();
        ui.on_dom_depth_changed(move |d| {
            let _ = tx.send(AppEvent::Ui(UiEvent::DomDepthChanged { depth: d }));
        });
    }
    {
        let tx = tx.clone();
        ui.on_render_mode_changed(move |full| {
            let _ = tx.send(AppEvent::Ui(UiEvent::RenderModeChanged { full }));
        });
    }
    {
        let tx = tx.clone();
        ui.on_history_valve_changed(move |open| {
            let _ = tx.send(AppEvent::Ui(UiEvent::HistoryValveChanged { open }));
        });
    }
    {
        let tx = tx.clone();
        ui.on_session_recording_changed(move |enabled| {
            let _ = tx.send(AppEvent::Ui(UiEvent::SessionRecordingChanged { enabled }));
        });
    }
    {
        let tx = tx.clone();
        ui.on_chart_view_mode_changed(move |mode| {
            let _ = tx.send(AppEvent::Ui(UiEvent::ChartViewModeChanged {
                mode: mode.to_string(),
            }));
        });
    }
    {
        let tx = tx.clone();
        ui.on_heatmap_enabled_changed(move |enabled| {
            let _ = tx.send(AppEvent::Ui(UiEvent::HeatmapEnabledChanged { enabled }));
        });
    }
    {
        let tx = tx.clone();
        ui.on_close_and_save_requested(move || {
            let _ = tx.send(AppEvent::Ui(UiEvent::CloseAndSaveRequested));
        });
    }
    {
        let tx = tx.clone();
        ui.on_draw_tool_changed(move |tool| {
            let _ = tx.send(AppEvent::Ui(UiEvent::DrawToolChanged {
                tool: tool.to_string(),
            }));
        });
    }
    {
        let tx = tx.clone();
        ui.on_draw_begin(move |x, y| {
            let _ = tx.send(AppEvent::Ui(UiEvent::DrawBegin { x, y }));
        });
    }
    {
        let tx = tx.clone();
        ui.on_draw_update(move |x, y| {
            let _ = tx.send(AppEvent::Ui(UiEvent::DrawUpdate { x, y }));
        });
    }
    {
        let tx = tx.clone();
        ui.on_draw_end(move |x, y| {
            let _ = tx.send(AppEvent::Ui(UiEvent::DrawEnd { x, y }));
        });
    }
    {
        let tx = tx.clone();
        ui.on_draw_poly_sides_delta(move |delta| {
            let _ = tx.send(AppEvent::Ui(UiEvent::DrawPolySidesDelta {
                delta: delta as i32,
            }));
        });
    }
    {
        let tx = tx.clone();
        ui.on_drawing_selected(move |id| {
            let _ = tx.send(AppEvent::Ui(UiEvent::DrawingSelected { id: id as u64 }));
        });
    }
    {
        let tx = tx.clone();
        ui.on_drawing_delete(move |id| {
            let _ = tx.send(AppEvent::Ui(UiEvent::DrawingDelete { id: id as u64 }));
        });
    }
    {
        let tx = tx.clone();
        ui.on_drawing_clear_all(move || {
            let _ = tx.send(AppEvent::Ui(UiEvent::DrawingClearAll));
        });
    }
    {
        let tx = tx.clone();
        ui.on_market_poll_adjust(move |delta| {
            let _ = tx.send(AppEvent::Ui(UiEvent::MarketPollAdjust { delta }));
        });
    }
    {
        let tx = tx.clone();
        ui.on_ticker_feed_toggled(move |ticker, enabled| {
            let _ = tx.send(AppEvent::Ui(UiEvent::TickerFeedToggled {
                ticker: ticker.to_string(),
                enabled,
            }));
        });
    }
    {
        let tx = tx.clone();
        ui.on_settings_connect_wallet(move || {
            let _ = tx.send(AppEvent::Ui(UiEvent::SettingsConnectWallet));
        });
    }
    {
        let tx = tx.clone();
        ui.on_settings_disconnect_wallet(move || {
            let _ = tx.send(AppEvent::Ui(UiEvent::SettingsDisconnectWallet));
        });
    }
    {
        let tx = tx.clone();
        ui.on_settings_refresh_status(move || {
            let _ = tx.send(AppEvent::Ui(UiEvent::SettingsRefreshStatus));
        });
    }
    {
        let tx = tx.clone();
        ui.on_settings_select_network(move |net| {
            let _ = tx.send(AppEvent::Ui(UiEvent::SettingsSelectNetwork {
                net: net.to_string(),
            }));
        });
    }
    {
        let tx = tx.clone();
        ui.on_settings_apply_rpc(move |endpoint| {
            let _ = tx.send(AppEvent::Ui(UiEvent::SettingsApplyRpc {
                endpoint: endpoint.to_string(),
            }));
        });
    }
    {
        let tx = tx.clone();
        ui.on_settings_toggle_auto_sign(move |enabled| {
            let _ = tx.send(AppEvent::Ui(UiEvent::SettingsToggleAutoSign { enabled }));
        });
    }
    {
        let tx = tx.clone();
        ui.on_settings_create_session(move |ttl_minutes| {
            let _ = tx.send(AppEvent::Ui(UiEvent::SettingsCreateSession {
                ttl_minutes: ttl_minutes.to_string(),
            }));
        });
    }
    {
        let tx = tx.clone();
        ui.on_settings_revoke_session(move || {
            let _ = tx.send(AppEvent::Ui(UiEvent::SettingsRevokeSession));
        });
    }
    {
        let tx = tx.clone();
        let ui_handle = ui.as_weak();
        ui.on_settings_copy_error(move || {
            if let Some(ui) = ui_handle.upgrade() {
                let text = ui.get_settings_last_error().to_string();
                if !text.trim().is_empty() {
                    let _ = i_slint_backend_selector::with_platform(|platform| {
                        platform.set_clipboard_text(
                            &text,
                            slint::platform::Clipboard::DefaultClipboard,
                        );
                        Ok(())
                    });
                }
            }
            let _ = tx.send(AppEvent::Ui(UiEvent::SettingsCopyError));
        });
    }
    {
        let tx = tx.clone();
        ui.on_ticker_favorite_toggled(move |ticker, favorite| {
            let _ = tx.send(AppEvent::Ui(UiEvent::TickerFavoriteToggled {
                ticker: ticker.to_string(),
                favorite,
            }));
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
    {
        let tx = tx.clone();
        ui.on_trade_size_text_changed(move |text| {
            let _ = tx.send(AppEvent::Ui(UiEvent::TradeSizeTextChanged {
                text: text.to_string(),
            }));
        });
    }
    {
        let tx = tx.clone();
        ui.on_trade_size_changed(move |value| {
            let _ = tx.send(AppEvent::Ui(UiEvent::TradeSizeChanged { value }));
        });
    }
    {
        let tx = tx.clone();
        ui.on_trade_leverage_text_changed(move |text| {
            let _ = tx.send(AppEvent::Ui(UiEvent::TradeLeverageTextChanged {
                text: text.to_string(),
            }));
        });
    }
    {
        let tx = tx.clone();
        ui.on_trade_leverage_changed(move |value| {
            let _ = tx.send(AppEvent::Ui(UiEvent::TradeLeverageChanged { value }));
        });
    }
    {
        let tx = tx.clone();
        ui.on_trade_margin_text_changed(move |text| {
            let _ = tx.send(AppEvent::Ui(UiEvent::TradeMarginTextChanged {
                text: text.to_string(),
            }));
        });
    }
    {
        let tx = tx.clone();
        ui.on_trade_margin_changed(move |value| {
            let _ = tx.send(AppEvent::Ui(UiEvent::TradeMarginChanged { value }));
        });
    }
    {
        let tx = tx.clone();
        ui.on_trade_margin_link_toggled(move |linked| {
            let _ = tx.send(AppEvent::Ui(UiEvent::TradeMarginLinkToggled { linked }));
        });
    }
    {
        let tx = tx.clone();
        ui.on_close_position_requested(move || {
            let _ = tx.send(AppEvent::Ui(UiEvent::ClosePositionRequested));
        });
    }
    {
        let tx = tx.clone();
        ui.on_cancel_open_orders(move || {
            let _ = tx.send(AppEvent::Ui(UiEvent::CancelOpenOrdersRequested));
        });
    }
    {
        let ui_handle = ui.as_weak();
        ui.on_stretch_to_viewport(move || {
            if let Some(ui) = ui_handle.upgrade() {
                let size = ui.window().size();
                ui.set_ui_content_w_px(size.width as f32);
                ui.set_ui_content_h_px(size.height as f32);
            }
        });
    }
    {
        let ui_handle = ui.as_weak();
        ui.on_fullscreen_toggle(move |state| {
            if let Some(ui) = ui_handle.upgrade() {
                let target = if state {
                    PhysicalSize::new(2400, 1600)
                } else {
                    PhysicalSize::new(1600, 1000)
                };
                ui.window().set_size(target);
                ui.set_ui_content_w_px(target.width as f32);
                ui.set_ui_content_h_px(target.height as f32);
            }
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
