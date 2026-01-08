use super::event::*;
use super::state::*;
use crate::debug_hooks;
use std::cmp::min;
use std::sync::atomic::Ordering;

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
            if !state.available_tickers.contains(&ticker) {
                state.order_message = format!("Ticker {} is not available.", ticker);
                return false;
            }
            if !state.ticker_active.get(&ticker).copied().unwrap_or(true) {
                state.order_message = format!("Ticker {} is inactive.", ticker);
                return false;
            }
            state.current_ticker = ticker;
            state.order_message = if state.history_valve_open {
                "Ticker changed; loading history.".to_string()
            } else {
                "Ticker changed; history paused.".to_string()
            };
            if let Ok(mut guard) = state.market_poll_ticker.lock() {
                *guard = state.current_ticker.clone();
            }

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
            state.best_bid_raw.clear();
            state.best_ask_raw.clear();
            state.mark_price_raw.clear();
            state.oracle_price_raw.clear();
            state.last_price_raw.clear();
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
        UiEvent::MarketPollAdjust { delta } => {
            let current = state.market_poll_secs as i64;
            let next = (current + delta as i64).clamp(1, 60) as u64;
            state.market_poll_secs = next;
            state.market_poll_interval.store(next, Ordering::Relaxed);
            state.order_message = format!("Market poll: {}s", next);
            true
        }
        UiEvent::TickerFeedToggled { ticker, enabled } => {
            if !ticker.is_empty() {
                if !state.ticker_active.get(&ticker).copied().unwrap_or(true) {
                    state.order_message = format!("Ticker {} is inactive.", ticker);
                    return false;
                }
                state.ticker_feed_enabled.insert(ticker.clone(), enabled);
                state.order_message = format!(
                    "Feed {}: {}",
                    ticker,
                    if enabled { "On" } else { "Off" }
                );
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
        UiEvent::ChartViewModeChanged { mode } => {
            state.chart_view_mode = mode;
            true
        }
        UiEvent::HeatmapEnabledChanged { enabled } => {
            state.heatmap_enabled = enabled;
            true
        }
        UiEvent::CloseAndSaveRequested => {
            match state.save_session_summary() {
                Ok(path) => {
                    state.order_message = format!("Session saved: {}", path.display());
                    state.close_after_save = true;
                }
                Err(err) => {
                    state.order_message = format!("Session save failed: {err}");
                }
            }
            true
        }
        UiEvent::DrawToolChanged { tool } => {
            state.draw_tool = tool;
            state.draw_active = None;
            true
        }
        UiEvent::DrawBegin { x, y } => {
            if state.draw_tool == "Pan" {
                return false;
            }
            let x = x.clamp(0.0, 1.0);
            let y = y.clamp(0.0, 1.0);
            let mut shape = DrawShapeState {
                id: state.next_draw_id(),
                kind: state.draw_tool.clone(),
                x1: x,
                y1: y,
                x2: x,
                y2: y,
                points: Vec::new(),
                sides: 0,
            };
            if shape.kind == "Poly" {
                shape.sides = 5;
            }
            if shape.kind == "Pencil" {
                shape.points.push(DrawPoint { x, y });
            }
            state.draw_active = Some(shape);
            true
        }
        UiEvent::DrawUpdate { x, y } => {
            let Some(active) = state.draw_active.as_mut() else {
                return false;
            };
            let x = x.clamp(0.0, 1.0);
            let y = y.clamp(0.0, 1.0);
            active.x2 = x;
            active.y2 = y;
            if active.kind == "Pencil" {
                let push = match active.points.last() {
                    Some(last) => {
                        let dx = x - last.x;
                        let dy = y - last.y;
                        (dx * dx + dy * dy).sqrt() >= 0.003
                    }
                    None => true,
                };
                if push {
                    active.points.push(DrawPoint { x, y });
                }
            }
            true
        }
        UiEvent::DrawEnd { x, y } => {
            let Some(mut active) = state.draw_active.take() else {
                return false;
            };
            let x = x.clamp(0.0, 1.0);
            let y = y.clamp(0.0, 1.0);
            active.x2 = x;
            active.y2 = y;
            if active.kind == "Pencil" {
                if let Some(last) = active.points.last() {
                    if (last.x - x).abs() > f32::EPSILON || (last.y - y).abs() > f32::EPSILON {
                        active.points.push(DrawPoint { x, y });
                    }
                } else {
                    active.points.push(DrawPoint { x, y });
                }
            }

            let dx = (active.x2 - active.x1).abs();
            let dy = (active.y2 - active.y1).abs();
            let should_keep = if active.kind == "Pencil" {
                active.points.len() > 1
            } else {
                dx >= 0.002 || dy >= 0.002
            };
            if should_keep {
                state.draw_selected_id = Some(active.id);
                state.drawings.push(active);
                if let Err(err) = state.save_session_drawings() {
                    state.order_message = format!("Drawings save failed: {err}");
                }
            }
            true
        }
        UiEvent::DrawPolySidesDelta { delta } => {
            let Some(active) = state.draw_active.as_mut() else {
                return false;
            };
            if active.kind != "Poly" {
                return false;
            }
            let mut sides = if active.sides >= 3 { active.sides as i32 } else { 5 };
            sides = (sides + delta).clamp(3, 12);
            active.sides = sides as u8;
            true
        }
        UiEvent::DrawingSelected { id } => {
            state.draw_selected_id = Some(id);
            true
        }
        UiEvent::DrawingDelete { id } => {
            let before = state.drawings.len();
            state.drawings.retain(|d| d.id != id);
            if state.draw_selected_id == Some(id) {
                state.draw_selected_id = None;
            }
            if before != state.drawings.len() {
                if let Err(err) = state.save_session_drawings() {
                    state.order_message = format!("Drawings save failed: {err}");
                }
                return true;
            }
            false
        }
        UiEvent::DrawingClearAll => {
            if state.drawings.is_empty() && state.draw_active.is_none() {
                return false;
            }
            state.drawings.clear();
            state.draw_active = None;
            state.draw_selected_id = None;
            if let Err(err) = state.save_session_drawings() {
                state.order_message = format!("Drawings save failed: {err}");
            }
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
        if !matches!(ev, FeedEvent::MarketList { .. }) {
            return false;
        }
    }
    match ev {
        FeedEvent::BookTop {
            ts_unix,
            ticker,
            best_bid,
            best_ask,
            best_bid_raw,
            best_ask_raw,
            bid_liq,
            ask_liq,
        } => {
            if !state.is_ticker_feed_enabled(&ticker) {
                return false;
            }
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
                if !best_bid_raw.is_empty() {
                    state.best_bid_raw = best_bid_raw;
                }
            }
            if best_ask > 0.0 {
                state.metrics.best_ask = best_ask;
                if !best_ask_raw.is_empty() {
                    state.best_ask_raw = best_ask_raw;
                }
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
            price,
            price_raw,
            source: _,
        } => {
            if !state.is_ticker_feed_enabled(&ticker) {
                return false;
            }
            if !ticker.is_empty() && ticker != state.current_ticker {
                debug_hooks::log_trade_skip(
                    "ticker_mismatch",
                    format!("state={} feed={}", state.current_ticker, ticker),
                );
                return false;
            }

            debug_hooks::log_trade_ingest(ts_unix, &ticker, &side, &size);

            if price.is_finite() && price > 0.0 {
                if !price_raw.is_empty() {
                    state.last_price_raw = price_raw.clone();
                } else {
                    state.last_price_raw = format_num_compact(price, PRICE_DECIMALS);
                }
            }

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
        FeedEvent::MarketPrice {
            ts_unix: _,
            ticker,
            mark_price,
            mark_price_raw,
            oracle_price,
            oracle_price_raw,
        } => {
            if !state.is_ticker_feed_enabled(&ticker) {
                return false;
            }
            if !ticker.is_empty() && ticker != state.current_ticker {
                return false;
            }
            if mark_price.is_finite() && mark_price > 0.0 {
                if !mark_price_raw.is_empty() {
                    state.mark_price_raw = mark_price_raw;
                } else {
                    state.mark_price_raw = format_num_compact(mark_price, PRICE_DECIMALS);
                }
            }
            if oracle_price.is_finite() && oracle_price > 0.0 {
                if !oracle_price_raw.is_empty() {
                    state.oracle_price_raw = oracle_price_raw;
                } else {
                    state.oracle_price_raw = format_num_compact(oracle_price, PRICE_DECIMALS);
                }
            }
            true
        }
        FeedEvent::BookLevels {
            ts_unix,
            ticker,
            bids,
            asks,
        } => {
            if !state.is_ticker_feed_enabled(&ticker) {
                return false;
            }
            let mut levels = Vec::with_capacity(bids.len() + asks.len());
            for b in &bids {
                levels.push(HeatmapLevel {
                    price: b.price,
                    size: b.size,
                    is_bid: true,
                });
            }
            for a in &asks {
                levels.push(HeatmapLevel {
                    price: a.price,
                    size: a.size,
                    is_bid: false,
                });
            }
            if !levels.is_empty() {
                state.record_heatmap_snapshot(HeatmapSnapshot {
                    ts_unix,
                    ticker: ticker.clone(),
                    levels,
                });
            }

            let is_current = ticker.is_empty() || ticker == state.current_ticker;
            if is_current && state.depth_enabled {
                let depth = state.dom_depth_levels as usize;
                state.bids = build_book_rows(&bids, true, depth);
                state.asks = build_book_rows(&asks, false, depth);
            }
            true
        }
        FeedEvent::MarketList { markets } => {
            if markets.is_empty() {
                return false;
            }
            let mut list: Vec<String> = Vec::with_capacity(markets.len());
            for market in &markets {
                list.push(market.ticker.clone());
                state
                    .ticker_active
                    .insert(market.ticker.clone(), market.active);
                let entry = state
                    .ticker_feed_enabled
                    .entry(market.ticker.clone())
                    .or_insert(false);
                if !market.active {
                    *entry = false;
                }
            }
            list.sort();
            list.dedup();
            state.available_tickers = list.clone();
            if !state.available_tickers.contains(&state.current_ticker) {
                state.available_tickers.insert(0, state.current_ticker.clone());
            }
            state
                .ticker_active
                .entry(state.current_ticker.clone())
                .or_insert(true);
            state
                .ticker_feed_enabled
                .entry(state.current_ticker.clone())
                .or_insert(false);
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
        let (price_main, price_pad) = split_number_value(px.max(0.0), PRICE_DECIMALS);
        let (size_main, size_pad) = split_number_value(sz.max(0.0), SIZE_DECIMALS);

        out.push(BookLevelRow {
            price_main,
            price_pad,
            size_main,
            size_pad,
            depth_ratio: ratio,
            is_best: i == 0,
        });
    }
    out
}

fn build_book_rows(levels: &[crate::feed_shared::BookLevel], is_bid: bool, depth: usize) -> Vec<BookLevelRow> {
    if levels.is_empty() || depth == 0 {
        return Vec::new();
    }
    let mut rows: Vec<_> = levels.iter().cloned().collect();
    rows.sort_by(|a, b| {
        if is_bid {
            b.price.partial_cmp(&a.price).unwrap_or(std::cmp::Ordering::Equal)
        } else {
            a.price.partial_cmp(&b.price).unwrap_or(std::cmp::Ordering::Equal)
        }
    });
    rows.truncate(depth.min(rows.len()));

    let max_size = rows
        .iter()
        .fold(0.0_f64, |acc, lvl| acc.max(lvl.size));

    let mut out = Vec::with_capacity(rows.len());
    for (idx, lvl) in rows.iter().enumerate() {
        let ratio = if max_size > 0.0 {
            (lvl.size / max_size).clamp(0.0, 1.0) as f32
        } else {
            0.0
        };
        let raw_price = if lvl.price_raw.is_empty() {
            format_num_compact(lvl.price, PRICE_DECIMALS)
        } else {
            lvl.price_raw.clone()
        };
        let raw_size = if lvl.size_raw.is_empty() {
            format_num_compact(lvl.size, SIZE_DECIMALS)
        } else {
            lvl.size_raw.clone()
        };
        let (price_main, price_pad) = split_number_raw(&raw_price, PRICE_DECIMALS);
        let (size_main, size_pad) = split_number_raw(&raw_size, SIZE_DECIMALS);
        out.push(BookLevelRow {
            price_main,
            price_pad,
            size_main,
            size_pad,
            depth_ratio: ratio,
            is_best: idx == 0,
        });
    }
    out
}
