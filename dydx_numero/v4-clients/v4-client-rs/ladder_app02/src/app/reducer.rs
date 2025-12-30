use super::event::*;
use super::state::*;
use std::cmp::min;

pub fn reduce(state: &mut AppState, ev: AppEvent) -> bool {
    match ev {
        AppEvent::Ui(u) => reduce_ui(state, u),
        AppEvent::Feed(f) => reduce_feed(state, f),
        AppEvent::Exec(x) => reduce_exec(state, x),
        AppEvent::Timer(t) => reduce_timer(state, t),
    }
}

fn reduce_ui(state: &mut AppState, ev: UiEvent) -> bool {
    match ev {
        UiEvent::TickerChanged { ticker } => {
            state.current_ticker = ticker;
            state.order_message = "Ticker changed.".to_string();

            // Optional: reset candles when ticker changes in scaffold mode
            state.reset_candles();

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

        UiEvent::CandleTfChanged { tf_secs } => {
            state.candle_tf_secs = tf_secs.max(1);
            state.order_message = format!("TF set to {}s", state.candle_tf_secs);

            // ✅ NEW API: reset and re-seed on next BookTop tick
            state.reset_candles();

            // If we already have a mid, seed immediately so chart pops instantly:
            if state.metrics.mid.is_finite() && state.metrics.mid > 0.0 {
                state.on_mid_tick(now_unix(), state.metrics.mid);
            }

            true
        }
        UiEvent::CandleWindowChanged { window_min } => {
            state.candle_window_minutes = window_min.max(1);
            state.order_message = format!("Window set to {}m", state.candle_window_minutes);

            // ✅ NEW API
            state.reset_candles();
            if state.metrics.mid.is_finite() && state.metrics.mid > 0.0 {
                state.on_mid_tick(now_unix(), state.metrics.mid);
            }

            true
        }
        UiEvent::DomDepthChanged { depth } => {
            state.dom_depth_levels = depth.clamp(5, 50);
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
    match ev {
        FeedEvent::BookTop {
            ts_unix,
            ticker,
            best_bid,
            best_ask,
            bid_liq,
            ask_liq,
        } => {
            if !ticker.is_empty() && ticker != state.current_ticker {
                return false;
            }

            // Some daemon messages intermittently report one side as zero. Preserve the
            // last known good price instead of letting the UI fall back to 0.0 (which
            // produced fake ladders starting at 0 and prevented candles from updating).
            let best_bid = if best_bid > 0.0 {
                best_bid
            } else {
                state.metrics.best_bid
            };
            let best_ask = if best_ask > 0.0 {
                best_ask
            } else {
                state.metrics.best_ask
            };

            // Still nothing reliable? Skip this tick.
            if best_bid <= 0.0 || best_ask <= 0.0 {
                return false;
            }

            state.metrics.best_bid = best_bid;
            state.metrics.best_ask = best_ask;

            if best_bid > 0.0 && best_ask > 0.0 {
                state.metrics.mid = (best_bid + best_ask) * 0.5;
                state.metrics.spread = (best_ask - best_bid).max(0.0);
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
            state.on_mid_tick(ts_unix, state.metrics.mid);

            // Build placeholder ladder
            state.bids = build_fake_side(best_bid, bid_liq, true, state.dom_depth_levels as usize);
            state.asks = build_fake_side(best_ask, ask_liq, false, state.dom_depth_levels as usize);

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
                return false;
            }

            let ts = format_time_basic(ts_unix);
            let is_buy = side.to_ascii_lowercase().starts_with('b');
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

            // ✅ add volume to candles (best-effort parse)
            let sz = size.parse::<f64>().unwrap_or(0.0);
            if sz > 0.0 {
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
            state.current_time = format_time_basic(now_unix);

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

            true
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
