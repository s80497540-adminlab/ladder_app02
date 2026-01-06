use super::event::*;
use super::state::*;
use crate::debug_hooks;
use std::cmp::min;

pub fn reduce(state: &mut AppState, ev: AppEvent) -> bool {
    match ev {
        AppEvent::Ui(u) => reduce_ui(state, u),
        AppEvent::Feed(f) => reduce_feed(state, f),
        AppEvent::Exec(x) => reduce_exec(state, x),
        AppEvent::Timer(t) => reduce_timer(state, t),
        AppEvent::HistoryLoaded { ticker, ticks, full } => {
            if !state.chart_enabled || !state.history_valve_open {
                return false;
            }
            if ticker != state.current_ticker {
                return false;
            }
            if state.render_all_candles != full {
                return false;
            }
            state.reset_candles();
            state.mid_ticks.clear();
            state.pending_mid_ticks = ticks.into();
            state.history_total = state.pending_mid_ticks.len();
            state.history_done = 0;
            state.history_loading = state.history_total > 0;
            state.history_load_full = full;
            state.order_message = if state.history_total > 0 {
                "History loading...".to_string()
            } else {
                "History empty.".to_string()
            };
            true
        }
    }
}

fn reduce_ui(state: &mut AppState, ev: UiEvent) -> bool {
    match ev {
        UiEvent::TickerChanged { ticker } => {
            state.current_ticker = ticker;
            state.order_message = if state.history_valve_open {
                "Ticker changed; loading history.".to_string()
            } else {
                "Ticker changed; history paused.".to_string()
            };

            debug_hooks::log_candle_reset("ticker changed; dropping cached candles");

            // Reset state for new ticker and reload any cached mids for it
            state.reset_candles();
            state.mid_ticks.clear();
            state.pending_mid_ticks.clear();
            state.history_loading = state.history_valve_open && state.chart_enabled;
            state.history_load_full = state.render_all_candles;
            state.history_total = 0;
            state.history_done = 0;
            state.metrics = Metrics::default();
            state.bids.clear();
            state.asks.clear();
            state.recent_trades.clear();

            if state.chart_enabled && !state.history_valve_open {
                if state.load_session_ticks_for_view(state.render_all_candles) {
                    state.rebuild_candles_from_history();
                }
            }

            true
        }
        UiEvent::ModeChanged { mode } => {
            state.mode = mode;
            state.order_message = "Mode changed.".to_string();
            true
        }
        UiEvent::TimeModeChanged { time_mode } => {
            state.time_mode = time_mode;
            state.order_message = "Time mode changed.".to_string();
            true
        }
        UiEvent::FeedEnabledChanged { enabled } => {
            state.feed_enabled = enabled;
            state.order_message = if enabled {
                "Feed enabled.".to_string()
            } else {
                "Feed paused.".to_string()
            };
            true
        }
        UiEvent::ChartEnabledChanged { enabled } => {
            state.chart_enabled = enabled;
            if enabled {
                state.order_message = if state.history_valve_open {
                    "Chart enabled; loading history.".to_string()
                } else {
                    "Chart enabled.".to_string()
                };
            } else {
                state.reset_candles();
                state.mid_ticks.clear();
                state.pending_mid_ticks.clear();
                state.history_loading = false;
                state.history_total = 0;
                state.history_done = 0;
                state.order_message = "Chart paused.".to_string();
            }
            true
        }
        UiEvent::DepthPanelToggled { enabled } => {
            state.depth_enabled = enabled;
            if !enabled {
                state.bids.clear();
                state.asks.clear();
            }
            true
        }
        UiEvent::TradesPanelToggled { enabled } => {
            state.trades_enabled = enabled;
            if !enabled {
                state.recent_trades.clear();
            }
            true
        }
        UiEvent::VolumePanelToggled { enabled } => {
            state.volume_enabled = enabled;
            true
        }

        UiEvent::CandleTfChanged { tf_secs } => {
            state.candle_tf_secs = tf_secs.max(1);
            state.order_message = format!("TF set to {}s", state.candle_tf_secs);

            debug_hooks::log_candle_reset("TF changed; rebuilding candles for new bucket size");
            if state.chart_enabled {
                if !state.history_valve_open {
                    state.load_session_ticks_for_view(state.render_all_candles);
                }
                state.rebuild_candles_from_history();
            }

            true
        }
        UiEvent::CandleWindowChanged { window_min } => {
            state.candle_window_minutes = window_min.max(1);
            state.order_message = format!("Window set to {}m", state.candle_window_minutes);

            // ✅ Rebuild cache under new window
            debug_hooks::log_candle_reset("window changed; rebuilding candle cache");
            if state.chart_enabled {
                if !state.history_valve_open {
                    state.load_session_ticks_for_view(state.render_all_candles);
                }
                state.rebuild_candles_from_history();
            }

            true
        }
        UiEvent::CandlePriceModeChanged { mode } => {
            state.candle_price_mode = mode;
            state.order_message = format!("Price mode: {}", state.candle_price_mode);

            debug_hooks::log_candle_reset("price mode changed; rebuilding candles");
            if state.chart_enabled {
                if !state.history_valve_open {
                    state.load_session_ticks_for_view(state.render_all_candles);
                }
                state.rebuild_candles_from_history();
            }

            true
        }
        UiEvent::DomDepthChanged { depth } => {
            state.dom_depth_levels = depth.clamp(5, 50);
            true
        }
        UiEvent::RenderModeChanged { full } => {
            state.render_all_candles = full;
            state.order_message = if full {
                "Render mode: full candles."
            } else {
                "Render mode: condensed view."
            }
            .to_string();

            state.mid_ticks.clear();
            state.pending_mid_ticks.clear();
            state.history_loading = state.history_valve_open && state.chart_enabled;
            state.history_load_full = full;
            state.history_total = 0;
            state.history_done = 0;
            if !full {
                state.candle_points.clear();
                state.candle_midline = 0.5;
            }
            if state.chart_enabled && !state.history_valve_open {
                if state.load_session_ticks_for_view(state.render_all_candles) {
                    state.rebuild_candles_from_history();
                }
            }
            true
        }
        UiEvent::HistoryValveChanged { open } => {
            state.history_valve_open = open;
            state.order_message = if open {
                "History valve opened.".to_string()
            } else {
                "History valve closed.".to_string()
            };
            true
        }
        UiEvent::SessionRecordingChanged { enabled } => {
            state.session_recording = enabled;
            state.order_message = if enabled {
                state.ensure_session_dir();
                "Session recording enabled.".to_string()
            } else {
                "Session recording paused.".to_string()
            };
            true
        }

        UiEvent::Deposit { amount } => {
            let a = amount.max(0.0);
            state.balance_usdc += a;
            state.order_message = format!("Deposited {:.2}", a);
            true
        }
        UiEvent::Withdraw { amount } => {
            let a = amount.max(0.0);
            state.balance_usdc = (state.balance_usdc - a).max(0.0);
            state.order_message = format!("Withdrew {:.2}", a);
            true
        }

        UiEvent::TradeRealModeToggled { enabled } => {
            state.trade_real_mode = enabled;
            if !enabled {
                state.trade_real_armed = false;
                state.trade_real_arm_expires_at = None;
                state.trade_real_arm_status = "NOT ARMED".to_string();
            }
            state.order_message = if enabled {
                "REAL enabled (needs ARM)."
            } else {
                "REAL disabled."
            }
            .to_string();
            true
        }

        UiEvent::ArmRealRequest { phrase } => {
            state.trade_real_arm_phrase = phrase.clone();

            if !state.trade_real_mode {
                state.trade_real_armed = false;
                state.trade_real_arm_status = "REAL OFF".to_string();
                state.order_message = "Enable REAL first.".to_string();
                return true;
            }

            if phrase.trim().eq_ignore_ascii_case("ARM") {
                let now = now_unix();
                state.trade_real_armed = true;
                state.trade_real_arm_expires_at = Some(now + 60);
                state.trade_real_arm_status = "ARMED (60s)".to_string();
                state.order_message = "REAL ARMED for 60 seconds.".to_string();
            } else {
                state.trade_real_armed = false;
                state.trade_real_arm_expires_at = None;
                state.trade_real_arm_status = "NOT ARMED".to_string();
                state.order_message = "Arm phrase must be: ARM".to_string();
            }
            true
        }

        UiEvent::DisarmReal => {
            state.trade_real_armed = false;
            state.trade_real_arm_expires_at = None;
            state.trade_real_arm_status = "NOT ARMED".to_string();
            state.order_message = "Disarmed.".to_string();
            true
        }

        UiEvent::SendOrder => {
            let now = now_unix();
            let ts = format_time_basic(now);

            let is_real = state.trade_real_mode;
            let armed_ok = !is_real || state.trade_real_armed;

            if !armed_ok {
                state.order_message = "Blocked: REAL requires ARM.".to_string();
                push_receipt(
                    state,
                    ReceiptRow {
                        ts,
                        ticker: state.current_ticker.clone(),
                        side: state.trade_side.clone(),
                        kind: "ManualReal".to_string(),
                        size: format!("{:.8}", state.trade_size),
                        status: "fail".to_string(),
                        comment: "not armed".to_string(),
                    },
                );
                return true;
            }

            let kind = if is_real { "ManualReal" } else { "ManualSim" }.to_string();
            state.order_message = "Order submitted (scaffold).".to_string();
            push_receipt(
                state,
                ReceiptRow {
                    ts,
                    ticker: state.current_ticker.clone(),
                    side: state.trade_side.clone(),
                    kind,
                    size: format!("{:.8}", state.trade_size),
                    status: "submitted".to_string(),
                    comment: "phase2-scaffold".to_string(),
                },
            );
            true
        }

        UiEvent::ReloadData => {
            state.order_message = "Reload requested (Phase-2: resubscribe TBD).".to_string();
            true
        }

        UiEvent::RunScript => {
            state.order_message =
                "RunScript requested (Phase-2: move to AppEvent flow).".to_string();
            true
        }
    }
}

fn reduce_feed(state: &mut AppState, ev: FeedEvent) -> bool {
    if !state.feed_enabled {
        return false;
    }
    match ev {
        FeedEvent::BookTop {
            ts_unix,
            ticker,
            best_bid,
            best_ask,
            bid_liq,
            ask_liq,
        } => {
            let is_current = ticker.is_empty() || ticker == state.current_ticker;
            if !is_current {
                // Persist candle feed in the background for non-view tickers.
                let mut combined_bid = if best_bid > 0.0 { best_bid } else { best_ask };
                let mut combined_ask = if best_ask > 0.0 { best_ask } else { best_bid };
                if combined_bid <= 0.0 && combined_ask > 0.0 {
                    combined_bid = combined_ask;
                } else if combined_ask <= 0.0 && combined_bid > 0.0 {
                    combined_ask = combined_bid;
                }
                if combined_bid > 0.0 && combined_ask > 0.0 {
                    let mid = (combined_bid + combined_ask) * 0.5;
                    state.persist_mid_tick_for_ticker(&ticker, ts_unix, mid, combined_bid, combined_ask);
                }
                return false;
            }

            debug_hooks::log_book_ingest(ts_unix, &ticker, best_bid, best_ask, bid_liq, ask_liq);

            // Some daemon messages intermittently report only one side of the book.
            // Persist partial updates so a later tick with the opposite side can still
            // produce a valid mid/spread instead of getting stuck at 0.0.
            if best_bid > 0.0 {
                state.metrics.best_bid = best_bid;
            }
            if best_ask > 0.0 {
                state.metrics.best_ask = best_ask;
            }

            let mut combined_bid = state.metrics.best_bid;
            let mut combined_ask = state.metrics.best_ask;

            // If only one side is seen, synthesize the missing side so candles can advance.
            if combined_bid <= 0.0 && combined_ask > 0.0 {
                combined_bid = combined_ask;
            } else if combined_ask <= 0.0 && combined_bid > 0.0 {
                combined_ask = combined_bid;
            }

            // Still nothing reliable? Skip this tick but keep any partials we captured above.
            if combined_bid <= 0.0 || combined_ask <= 0.0 {
                debug_hooks::log_book_skip(
                    "invalid_prices",
                    format!(
                        "best_bid={} best_ask={} state_bid={} state_ask={}",
                        best_bid, best_ask, state.metrics.best_bid, state.metrics.best_ask
                    ),
                );
                return false;
            }

            if combined_bid > 0.0 && combined_ask > 0.0 {
                state.metrics.mid = (combined_bid + combined_ask) * 0.5;
                state.metrics.spread = (combined_ask - combined_bid).max(0.0);
            } else {
                state.metrics.mid = 0.0;
                state.metrics.spread = 0.0;
            }

            state.metrics.imbalance = if ask_liq > 0.0 {
                (bid_liq / ask_liq).max(0.0)
            } else {
                0.0
            };

            // ✅ NEW candle API: build candles off timestamp-bucketed mid ticks
            if state.chart_enabled {
                state.on_mid_tick(ts_unix, state.metrics.mid, state.metrics.best_bid, state.metrics.best_ask);
            } else {
                let ticker = state.current_ticker.clone();
                state.persist_mid_tick_for_ticker(
                    &ticker,
                    ts_unix,
                    state.metrics.mid,
                    state.metrics.best_bid,
                    state.metrics.best_ask,
                );
            }

            // Build placeholder ladder
            if state.depth_enabled {
                debug_hooks::log_placeholder_ladder(
                    best_bid,
                    best_ask,
                    state.dom_depth_levels as usize,
                    bid_liq,
                    ask_liq,
                );
                state.bids = build_fake_side(best_bid, bid_liq, true, state.dom_depth_levels as usize);
                state.asks = build_fake_side(best_ask, ask_liq, false, state.dom_depth_levels as usize);
            }

            true
        }
        FeedEvent::Trade {
            ts_unix,
            ticker,
            side,
            size,
            source: _,
        } => {
            if !ticker.is_empty() && ticker != state.current_ticker {
                debug_hooks::log_trade_skip(
                    "ticker_mismatch",
                    format!("state={} feed={}", state.current_ticker, ticker),
                );
                return false;
            }

            debug_hooks::log_trade_ingest(ts_unix, &ticker, &side, &size);

            let ts = format_time_basic(ts_unix);
            let is_buy = side.to_ascii_lowercase().starts_with('b');
            if state.trades_enabled {
                state.recent_trades.push(TradeRow {
                    ts,
                    side: side.clone(),
                    size: size.clone(),
                    is_buy,
                });

                // cap trades
                if state.recent_trades.len() > 60 {
                    let extra = state.recent_trades.len() - 60;
                    state.recent_trades.drain(0..extra);
                }
            }

            // ✅ add volume to candles (best-effort parse)
            let sz = size.parse::<f64>().unwrap_or(0.0);
            if sz > 0.0 && state.volume_enabled && state.chart_enabled {
                state.on_trade_volume(ts_unix, sz);
            }

            true
        }
    }
}

fn reduce_exec(state: &mut AppState, ev: ExecEvent) -> bool {
    match ev {
        ExecEvent::Receipt {
            ts,
            ticker,
            side,
            kind,
            size,
            status,
            comment,
        } => {
            push_receipt(
                state,
                ReceiptRow {
                    ts,
                    ticker,
                    side,
                    kind,
                    size,
                    status,
                    comment,
                },
            );
            true
        }
    }
}

fn reduce_timer(state: &mut AppState, ev: TimerEvent) -> bool {
    match ev {
        TimerEvent::Tick1s { now_unix } => {
            let mut changed = false;
            let new_time = format_time_basic(now_unix);
            if state.current_time != new_time {
                state.current_time = new_time;
                changed = true;
            }
            if state.update_daemon_status(now_unix) {
                changed = true;
            }

            // Arm expiry
            if let Some(exp) = state.trade_real_arm_expires_at {
                if now_unix >= exp {
                    state.trade_real_armed = false;
                    state.trade_real_arm_expires_at = None;
                    state.trade_real_arm_status = "NOT ARMED".to_string();
                    state.order_message = "ARM expired.".to_string();
                    return true;
                } else {
                    let left = exp - now_unix;
                    state.trade_real_arm_status = format!("ARMED ({}s)", left);
                    return true;
                }
            }

            changed
        }
    }
}

fn push_receipt(state: &mut AppState, r: ReceiptRow) {
    state.receipts.push(r);
    if state.receipts.len() > 300 {
        let extra = state.receipts.len() - 300;
        state.receipts.drain(0..extra);
    }
}

fn build_fake_side(best: f64, liq: f64, is_bid: bool, depth: usize) -> Vec<BookLevelRow> {
    let depth = min(depth.max(1), 50);
    let mut out = Vec::with_capacity(depth);

    let base_size = (liq / depth as f64).max(0.0001);
    for i in 0..depth {
        let px = if is_bid {
            best - (i as f64 * 0.5)
        } else {
            best + (i as f64 * 0.5)
        };
        let sz = base_size * (1.0 + (depth - i) as f64 / depth as f64);
        let ratio = ((depth - i) as f32 / depth as f32).clamp(0.0, 1.0);

        out.push(BookLevelRow {
            price: format!("{:.2}", px.max(0.0)),
            size: format!("{:.4}", sz.max(0.0)),
            depth_ratio: ratio,
            is_best: i == 0,
        });
    }
    out
}
