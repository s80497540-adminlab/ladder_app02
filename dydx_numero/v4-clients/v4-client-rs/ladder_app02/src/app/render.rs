use super::state::*;
use crate::{
    AxisTick, BookLevel, CandlePoint, CandleRow, DrawShape, DrawTick, HeatmapCell, 
    ImbalancePoint, LiquidityPoint, Receipt, SpreadPoint, Trade, TickerFeedRow, 
    VolumeProfileBar,
};
use chrono::{DateTime, NaiveDateTime, Utc};
use slint::{Color, ModelRc, SharedString, VecModel};

const MAX_CONDENSED_POINTS: usize = 600;

pub fn render(state: &AppState, ui: &crate::AppWindow) {
    // Core
    ui.set_current_ticker(SharedString::from(state.current_ticker.clone()));
    ui.set_mode(SharedString::from(state.mode.clone()));
    ui.set_time_mode(SharedString::from(state.time_mode.clone()));
    ui.set_market_poll_secs(state.market_poll_secs as i32);

    ui.set_candle_tf_secs(state.candle_tf_secs);
    ui.set_candle_window_minutes(state.candle_window_minutes);
    ui.set_candle_price_mode(SharedString::from(state.candle_price_mode.clone()));
    ui.set_dom_depth_levels(state.dom_depth_levels);
    ui.set_render_all_candles(state.render_all_candles);
    ui.set_feed_enabled(state.feed_enabled);
    ui.set_chart_enabled(state.chart_enabled);
    ui.set_chart_view_mode(SharedString::from(state.chart_view_mode.clone()));
    ui.set_heatmap_enabled(state.heatmap_enabled);
    ui.set_session_recording(state.session_recording);
    
    // Chart toggles
    ui.set_show_volume_profile(state.show_volume_profile);
    ui.set_show_liquidity_chart(state.show_liquidity_chart);
    ui.set_show_imbalance_chart(state.show_imbalance_chart);
    ui.set_show_spread_chart(state.show_spread_chart);
    
    // Normalization modes
    ui.set_volume_profile_normalize(state.volume_profile_normalize);
    ui.set_liquidity_normalize(state.liquidity_normalize);
    ui.set_imbalance_normalize(state.imbalance_normalize);
    ui.set_spread_normalize(state.spread_normalize);

    ui.set_trade_side(SharedString::from(state.trade_side.clone()));
    ui.set_trade_order_type(SharedString::from(state.trade_order_type.clone()));
    ui.set_trade_order_type(SharedString::from(state.trade_order_type.clone()));
    ui.set_trade_order_type(SharedString::from(state.trade_order_type.clone()));
    ui.set_trade_order_type(SharedString::from(state.trade_order_type.clone()));
    ui.set_trade_order_type(SharedString::from(state.trade_order_type.clone()));
    ui.set_trade_order_type(SharedString::from(state.trade_order_type.clone()));
    ui.set_trade_size(state.trade_size);
    ui.set_trade_leverage(state.trade_leverage);
    ui.set_trade_size_text(SharedString::from(state.trade_size_text.clone()));
    ui.set_trade_leverage_text(SharedString::from(state.trade_leverage_text.clone()));
    ui.set_trade_margin(state.trade_margin);
    ui.set_trade_margin_text(SharedString::from(state.trade_margin_text.clone()));
    ui.set_trade_margin_linked(state.trade_margin_linked);
    ui.set_trade_limit_price(state.trade_limit_price);
    ui.set_trade_limit_price_text(SharedString::from(state.trade_limit_price_text.clone()));
    ui.set_trade_trigger_price(state.trade_trigger_price);
    ui.set_trade_trigger_price_text(SharedString::from(state.trade_trigger_price_text.clone()));
    ui.set_trade_post_only(state.trade_post_only);
    ui.set_trade_time_in_force(SharedString::from(state.trade_time_in_force.clone()));
    ui.set_trade_limit_price(state.trade_limit_price);
    ui.set_trade_limit_price_text(SharedString::from(state.trade_limit_price_text.clone()));
    ui.set_trade_trigger_price(state.trade_trigger_price);
    ui.set_trade_trigger_price_text(SharedString::from(state.trade_trigger_price_text.clone()));
    ui.set_trade_post_only(state.trade_post_only);
    ui.set_trade_time_in_force(SharedString::from(state.trade_time_in_force.clone()));
    ui.set_trade_limit_price(state.trade_limit_price);
    ui.set_trade_limit_price_text(SharedString::from(state.trade_limit_price_text.clone()));
    ui.set_trade_trigger_price(state.trade_trigger_price);
    ui.set_trade_trigger_price_text(SharedString::from(state.trade_trigger_price_text.clone()));
    ui.set_trade_post_only(state.trade_post_only);
    ui.set_trade_time_in_force(SharedString::from(state.trade_time_in_force.clone()));
    ui.set_trade_limit_price(state.trade_limit_price);
    ui.set_trade_limit_price_text(SharedString::from(state.trade_limit_price_text.clone()));
    ui.set_trade_trigger_price(state.trade_trigger_price);
    ui.set_trade_trigger_price_text(SharedString::from(state.trade_trigger_price_text.clone()));
    ui.set_trade_post_only(state.trade_post_only);
    ui.set_trade_time_in_force(SharedString::from(state.trade_time_in_force.clone()));
    ui.set_trade_limit_price(state.trade_limit_price);
    ui.set_trade_limit_price_text(SharedString::from(state.trade_limit_price_text.clone()));
    ui.set_trade_trigger_price(state.trade_trigger_price);
    ui.set_trade_trigger_price_text(SharedString::from(state.trade_trigger_price_text.clone()));
    ui.set_trade_post_only(state.trade_post_only);
    ui.set_trade_time_in_force(SharedString::from(state.trade_time_in_force.clone()));
    ui.set_trade_limit_price(state.trade_limit_price);
    ui.set_trade_limit_price_text(SharedString::from(state.trade_limit_price_text.clone()));
    ui.set_trade_trigger_price(state.trade_trigger_price);
    ui.set_trade_trigger_price_text(SharedString::from(state.trade_trigger_price_text.clone()));
    ui.set_trade_post_only(state.trade_post_only);
    ui.set_trade_time_in_force(SharedString::from(state.trade_time_in_force.clone()));

    ui.set_trade_real_mode(state.trade_real_mode);
    ui.set_trade_real_armed(state.trade_real_armed);
    ui.set_trade_real_arm_status(SharedString::from(state.trade_real_arm_status.clone()));

    ui.set_balance_usdc(state.balance_usdc);
    ui.set_balance_pnl(state.balance_pnl);
    ui.set_account_equity_text(SharedString::from(state.account_equity_text.clone()));
    ui.set_account_free_collateral_text(SharedString::from(
        state.account_free_collateral_text.clone(),
    ));
    ui.set_account_status(SharedString::from(state.account_status.clone()));
    ui.set_position_status_text(SharedString::from(state.position_status_text.clone()));
    ui.set_open_orders_text(SharedString::from(state.open_orders_text.clone()));

    ui.set_current_time(SharedString::from(state.current_time.clone()));
    ui.set_order_message(SharedString::from(state.order_message.clone()));
    ui.set_last_order_status_text(SharedString::from(
        state.last_order_status_text.clone(),
    ));
    ui.set_axis_price_unit(SharedString::from(state.current_ticker.clone()));
    ui.set_candle_feed_status(SharedString::from(state.candle_feed_status()));
    ui.set_history_status(SharedString::from(state.history_status()));
    ui.set_perf_status(SharedString::from(state.perf_status()));
    ui.set_perf_load(state.perf_load);
    ui.set_perf_healthy(state.perf_healthy);
    ui.set_daemon_status(SharedString::from(state.daemon_status.clone()));
    ui.set_daemon_active(state.daemon_active);
    ui.set_draw_tool(SharedString::from(state.draw_tool.clone()));
    ui.set_draw_selected_id(state.draw_selected_id.map(|id| id as i32).unwrap_or(-1));
    ui.set_settings_wallet_address(SharedString::from(
        state.settings_wallet_address.clone(),
    ));
    ui.set_settings_wallet_status(SharedString::from(
        state.settings_wallet_status.clone(),
    ));
    ui.set_settings_network(SharedString::from(state.settings_network.clone()));
    ui.set_settings_rpc_endpoint(SharedString::from(
        state.settings_rpc_endpoint.clone(),
    ));
    ui.set_settings_auto_sign(state.settings_auto_sign);
    ui.set_settings_session_ttl_minutes(SharedString::from(
        state.settings_session_ttl_minutes.clone(),
    ));
    ui.set_settings_signer_status(SharedString::from(
        state.settings_signer_status.clone(),
    ));
    ui.set_settings_last_error(SharedString::from(state.settings_last_error.clone()));

    let mut rows: Vec<TickerFeedRow> = Vec::new();
    let mut tickers = state.available_tickers.clone();
    tickers.sort();
    tickers.dedup();
    tickers.sort_by(|a, b| {
        let fa = state.ticker_favorites.get(a).copied().unwrap_or(false);
        let fb = state.ticker_favorites.get(b).copied().unwrap_or(false);
        if fa != fb {
            return fb.cmp(&fa);
        }
        a.cmp(b)
    });
    for tk in tickers {
        let enabled = state.ticker_feed_enabled.get(&tk).copied().unwrap_or(true);
        let active = state.ticker_active.get(&tk).copied().unwrap_or(true);
        let favorite = state.ticker_favorites.get(&tk).copied().unwrap_or(false);
        rows.push(TickerFeedRow {
            ticker: SharedString::from(tk),
            feed_on: enabled,
            active,
            favorite,
        });
    }
    ui.set_ticker_feed_rows(ModelRc::new(VecModel::from(rows)));

    // Metrics
    ui.set_mid_price(state.metrics.mid as f32);
    ui.set_best_bid(state.metrics.best_bid as f32);
    ui.set_best_ask(state.metrics.best_ask as f32);
    ui.set_spread(state.metrics.spread as f32);
    ui.set_imbalance(state.metrics.imbalance as f32);
    let (mid_main, mid_pad) = split_number_value(state.metrics.mid, PRICE_DECIMALS);
    let (bid_main, bid_pad) = if state.best_bid_raw.is_empty() {
        split_number_value(state.metrics.best_bid, PRICE_DECIMALS)
    } else {
        split_number_raw(&state.best_bid_raw, PRICE_DECIMALS)
    };
    let (ask_main, ask_pad) = if state.best_ask_raw.is_empty() {
        split_number_value(state.metrics.best_ask, PRICE_DECIMALS)
    } else {
        split_number_raw(&state.best_ask_raw, PRICE_DECIMALS)
    };
    let (spread_main, spread_pad) = split_number_value(state.metrics.spread, PRICE_DECIMALS);

    ui.set_mid_price_main(SharedString::from(mid_main));
    ui.set_mid_price_pad(SharedString::from(mid_pad));
    ui.set_best_bid_main(SharedString::from(bid_main));
    ui.set_best_bid_pad(SharedString::from(bid_pad));
    ui.set_best_ask_main(SharedString::from(ask_main));
    ui.set_best_ask_pad(SharedString::from(ask_pad));
    ui.set_spread_main(SharedString::from(spread_main));
    ui.set_spread_pad(SharedString::from(spread_pad));

    let (cur_main, cur_pad) = match state.candle_price_mode.as_str() {
        "Bid" if !state.best_bid_raw.is_empty() => {
            split_number_raw(&state.best_bid_raw, PRICE_DECIMALS)
        }
        "Ask" if !state.best_ask_raw.is_empty() => {
            split_number_raw(&state.best_ask_raw, PRICE_DECIMALS)
        }
        "Trade" if !state.last_price_raw.is_empty() => {
            split_number_raw(&state.last_price_raw, PRICE_DECIMALS)
        }
        "Bid" if state.metrics.best_bid.is_finite() && state.metrics.best_bid > 0.0 => {
            split_number_value(state.metrics.best_bid, PRICE_DECIMALS)
        }
        "Ask" if state.metrics.best_ask.is_finite() && state.metrics.best_ask > 0.0 => {
            split_number_value(state.metrics.best_ask, PRICE_DECIMALS)
        }
        _ => split_number_value(state.metrics.mid, PRICE_DECIMALS),
    };
    ui.set_current_price_main(SharedString::from(cur_main));
    ui.set_current_price_pad(SharedString::from(cur_pad));

    let (mark_main, mark_pad) = if state.mark_price_raw.trim().is_empty() {
        (String::new(), String::new())
    } else {
        split_number_raw(&state.mark_price_raw, PRICE_DECIMALS)
    };
    let (oracle_main, oracle_pad) = if state.oracle_price_raw.trim().is_empty() {
        (String::new(), String::new())
    } else {
        split_number_raw(&state.oracle_price_raw, PRICE_DECIMALS)
    };
    let (last_main, last_pad) = if state.last_price_raw.trim().is_empty() {
        (String::new(), String::new())
    } else {
        split_number_raw(&state.last_price_raw, PRICE_DECIMALS)
    };
    ui.set_mark_price_main(SharedString::from(mark_main));
    ui.set_mark_price_pad(SharedString::from(mark_pad));
    ui.set_oracle_price_main(SharedString::from(oracle_main));
    ui.set_oracle_price_pad(SharedString::from(oracle_pad));
    ui.set_last_price_main(SharedString::from(last_main));
    ui.set_last_price_pad(SharedString::from(last_pad));

    // Book models
    let bids: Vec<BookLevel> = state
        .bids
        .iter()
        .map(|b| BookLevel {
            price_main: SharedString::from(b.price_main.clone()),
            price_pad: SharedString::from(b.price_pad.clone()),
            size_main: SharedString::from(b.size_main.clone()),
            size_pad: SharedString::from(b.size_pad.clone()),
            depth_ratio: b.depth_ratio,
            is_best: b.is_best,
        })
        .collect();
    ui.set_bids(ModelRc::new(VecModel::from(bids)));

    let asks: Vec<BookLevel> = state
        .asks
        .iter()
        .map(|a| BookLevel {
            price_main: SharedString::from(a.price_main.clone()),
            price_pad: SharedString::from(a.price_pad.clone()),
            size_main: SharedString::from(a.size_main.clone()),
            size_pad: SharedString::from(a.size_pad.clone()),
            depth_ratio: a.depth_ratio,
            is_best: a.is_best,
        })
        .collect();
    ui.set_asks(ModelRc::new(VecModel::from(asks)));

    // Trades
    let trades: Vec<Trade> = state
        .recent_trades
        .iter()
        .rev()
        .take(50)
        .map(|t| Trade {
            ts: SharedString::from(t.ts.clone()),
            side: SharedString::from(t.side.clone()),
            size: SharedString::from(t.size.clone()),
            is_buy: t.is_buy,
        })
        .collect();
    ui.set_recent_trades(ModelRc::new(VecModel::from(trades)));

    // Candles (rows under the chart)
    let candle_rows: Vec<CandleRow> = state
        .candles
        .iter()
        .rev()
        .take(500)
        .map(|c| CandleRow {
            ts: SharedString::from(c.ts.clone()),
            open: SharedString::from(format_num_compact(c.open, PRICE_DECIMALS)),
            high: SharedString::from(format_num_compact(c.high, PRICE_DECIMALS)),
            low: SharedString::from(format_num_compact(c.low, PRICE_DECIMALS)),
            close: SharedString::from(format_num_compact(c.close, PRICE_DECIMALS)),
            volume: SharedString::from(format_num_compact(c.volume, SIZE_DECIMALS)),
        })
        .collect();
    ui.set_candles(ModelRc::new(VecModel::from(candle_rows)));

    // Candle points (what CandleChart actually draws)
    let condensed_candles = if state.render_all_candles {
        None
    } else {
        Some(condense_candles(&state.candles, MAX_CONDENSED_POINTS))
    };
    let candles_for_view: &[Candle] = condensed_candles.as_deref().unwrap_or(&state.candles);

    let points: Vec<CandlePoint> = if state.render_all_candles {
        state
            .candle_points
            .iter()
            .map(|p| CandlePoint {
                x: p.x,
                w: p.w,
                open: p.open,
                high: p.high,
                low: p.low,
                close: p.close,
                is_up: p.is_up,
                volume: p.volume,
            })
            .collect()
    } else {
        build_candle_points_from_candles(candles_for_view)
    };
    ui.set_candle_points(ModelRc::new(VecModel::from(points)));

    let candle_midline = if state.render_all_candles {
        state.candle_midline
    } else {
        0.5
    };
    ui.set_candle_midline(candle_midline);
    let (entry_visible, entry_y, entry_color) = entry_line_state(state, candles_for_view);
    ui.set_position_entry_visible(entry_visible);
    ui.set_position_entry_y(entry_y);
    ui.set_position_entry_color(entry_color);

    let chart_x_zoom = ui.get_chart_x_zoom() as f64;
    let chart_y_zoom = ui.get_chart_y_zoom() as f64;
    let chart_pan_x = ui.get_chart_pan_x() as f64;
    let chart_pan_y = ui.get_chart_pan_y() as f64;

    // Axis ticks derived from visible window (pan/zoom aware)
    let price_ctx = price_axis_context(candles_for_view);
    let price_ticks = price_ctx
        .as_ref()
        .map(|ctx| build_price_ticks_visible(ctx, chart_y_zoom, chart_pan_y, &state.current_ticker))
        .unwrap_or_default();
    ui.set_price_ticks(ModelRc::new(VecModel::from(price_ticks)));

    let time_ctx = time_axis_context(candles_for_view);
    let time_ticks = time_ctx
        .as_ref()
        .map(|ctx| build_time_ticks_visible(ctx, chart_x_zoom, chart_pan_x))
        .unwrap_or_default();
    ui.set_time_ticks(ModelRc::new(VecModel::from(time_ticks)));

    let heatmap_cells = if state.chart_view_mode == "Advanced" && state.heatmap_enabled {
        build_heatmap_cells(
            state,
            price_ctx.as_ref(),
            time_ctx.as_ref(),
            chart_x_zoom,
            chart_pan_x,
            chart_y_zoom,
            chart_pan_y,
        )
    } else {
        Vec::new()
    };
    ui.set_heatmap_cells(ModelRc::new(VecModel::from(heatmap_cells)));

    // Render new indicator data
    render_volume_profile(state, ui);
    render_liquidity_history(state, ui);
    render_imbalance_history(state, ui);
    render_spread_history(state, ui);

    let mut drawings: Vec<DrawShape> = state
        .drawings
        .iter()
        .map(|d| {
            let selected = state.draw_selected_id == Some(d.id);
            build_draw_shape(
                d,
                false,
                selected,
                price_ctx.as_ref(),
                time_ctx.as_ref(),
                chart_x_zoom,
                chart_pan_x,
                chart_y_zoom,
                chart_pan_y,
            )
        })
        .collect();
    if let Some(active) = &state.draw_active {
        drawings.push(build_draw_shape(
            active,
            true,
            false,
            price_ctx.as_ref(),
            time_ctx.as_ref(),
            chart_x_zoom,
            chart_pan_x,
            chart_y_zoom,
            chart_pan_y,
        ));
    }
    ui.set_drawings(ModelRc::new(VecModel::from(drawings)));

    let mut draw_ticks: Vec<DrawTick> = Vec::new();
    for d in &state.drawings {
        if d.kind == "Ruler" {
            draw_ticks.extend(build_ruler_ticks(
                d,
                false,
                price_ctx.as_ref(),
                chart_y_zoom,
                chart_pan_y,
            ));
        }
    }
    if let Some(active) = &state.draw_active {
        if active.kind == "Ruler" {
            draw_ticks.extend(build_ruler_ticks(
                active,
                true,
                price_ctx.as_ref(),
                chart_y_zoom,
                chart_pan_y,
            ));
        }
    }
    ui.set_draw_ticks(ModelRc::new(VecModel::from(draw_ticks)));

    // Receipts
    let receipts: Vec<Receipt> = state
        .receipts
        .iter()
        .rev()
        .take(300)
        .map(|r| Receipt {
            ts: SharedString::from(r.ts.clone()),
            ticker: SharedString::from(r.ticker.clone()),
            side: SharedString::from(r.side.clone()),
            kind: SharedString::from(r.kind.clone()),
            size: SharedString::from(r.size.clone()),
            status: SharedString::from(r.status.clone()),
            comment: SharedString::from(r.comment.clone()),
        })
        .collect();
    ui.set_receipts(ModelRc::new(VecModel::from(receipts)));
}

fn condense_candles(candles: &[Candle], max_points: usize) -> Vec<Candle> {
    if candles.is_empty() || max_points == 0 || candles.len() <= max_points {
        return candles.to_vec();
    }

    let group = (candles.len() + max_points - 1) / max_points;
    let mut out = Vec::with_capacity((candles.len() + group - 1) / group);
    for chunk in candles.chunks(group) {
        if chunk.is_empty() {
            continue;
        }
        let first = &chunk[0];
        let last = &chunk[chunk.len() - 1];
        let mut high = f64::NEG_INFINITY;
        let mut low = f64::INFINITY;
        let mut volume = 0.0;
        for c in chunk {
            high = high.max(c.high);
            low = low.min(c.low);
            volume += c.volume;
        }
        out.push(Candle {
            ts: first.ts.clone(),
            open: first.open,
            high,
            low,
            close: last.close,
            volume,
        });
    }
    out
}

fn build_candle_points_from_candles(candles: &[Candle]) -> Vec<CandlePoint> {
    if candles.is_empty() {
        return Vec::new();
    }

    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    let mut vmax: f64 = 0.0;

    for c in candles {
        lo = lo.min(c.low);
        hi = hi.max(c.high);
        vmax = vmax.max(c.volume);
    }

    let mut span = hi - lo;
    if !span.is_finite() || span <= 0.0 {
        span = hi.abs().max(1.0);
        lo = hi - span;
    }
    let pad = span * 0.02;
    lo -= pad;
    hi += pad;
    let span = (hi - lo).max(1e-9);

    let y = |price: f64| -> f32 { ((hi - price) / span).clamp(0.0, 1.0) as f32 };

    let n = candles.len().max(1);
    let w = (1.0 / n as f32).clamp(0.01, 0.2);

    candles
        .iter()
        .enumerate()
        .map(|(i, c)| CandlePoint {
            x: (i as f32 + 0.5) / n as f32,
            w,
            open: y(c.open),
            high: y(c.high),
            low: y(c.low),
            close: y(c.close),
            is_up: c.close >= c.open,
            volume: if vmax > 0.0 {
                (c.volume / vmax).clamp(0.0, 1.0) as f32
            } else {
                0.0
            },
        })
        .collect()
}

fn padded_price_range(candles: &[Candle]) -> Option<(f64, f64, f64)> {
    if candles.is_empty() {
        return None;
    }
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for c in candles {
        lo = lo.min(c.low);
        hi = hi.max(c.high);
    }
    if !lo.is_finite() || !hi.is_finite() {
        return None;
    }
    let mut span = hi - lo;
    if !span.is_finite() || span <= 0.0 {
        span = hi.abs().max(1.0);
        lo = hi - span;
    }
    let pad = span * 0.02;
    lo -= pad;
    hi += pad;
    let span = (hi - lo).max(1e-9);
    Some((lo, hi, span))
}

fn entry_line_state(state: &AppState, candles: &[Candle]) -> (bool, f32, Color) {
    let mut color = Color::from_rgb_u8(102, 204, 136);
    let side = state.position_side.to_ascii_lowercase();
    if side == "short" {
        color = Color::from_rgb_u8(204, 102, 119);
    } else if side != "long" {
        color = Color::from_rgb_u8(160, 170, 190);
    }

    let entry = state.position_entry as f64;
    if state.position_size <= 0.0
        || entry <= 0.0
        || !entry.is_finite()
        || side == "flat"
        || (!state.position_ticker.is_empty()
            && !state
                .position_ticker
                .eq_ignore_ascii_case(&state.current_ticker))
    {
        return (false, 0.0, color);
    }

    let Some((_lo, hi, span)) = padded_price_range(candles) else {
        return (false, 0.0, color);
    };
    let y = ((hi - entry) / span).clamp(0.0, 1.0) as f32;
    (true, y, color)
}

fn parse_unix_ts(ts: &str) -> Option<u64> {
    ts.strip_prefix("unix:")?.parse().ok()
}

fn format_utc(ts_unix: u64) -> Option<String> {
    let dt = NaiveDateTime::from_timestamp_opt(ts_unix as i64, 0)?;
    let dt: DateTime<Utc> = DateTime::from_naive_utc_and_offset(dt, Utc);
    Some(dt.format("%H:%M:%S UTC").to_string())
}

#[derive(Clone, Debug)]
struct PriceAxisContext {
    lo: f64,
    hi: f64,
    span: f64,
    decimals: usize,
}

#[derive(Clone, Debug)]
struct TimeAxisContext {
    ts: Vec<u64>,
}

fn price_axis_context(candles: &[Candle]) -> Option<PriceAxisContext> {
    if candles.is_empty() {
        return None;
    }
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for c in candles {
        lo = lo.min(c.low);
        hi = hi.max(c.high);
    }
    if !lo.is_finite() || !hi.is_finite() || hi <= lo {
        return None;
    }
    let span = hi - lo;
    let decimals = price_decimals(span);
    Some(PriceAxisContext {
        lo,
        hi,
        span,
        decimals,
    })
}

fn price_decimals(span: f64) -> usize {
    if span >= 1000.0 {
        0
    } else if span >= 100.0 {
        1
    } else if span >= 10.0 {
        2
    } else if span >= 1.0 {
        3
    } else if span >= 0.1 {
        4
    } else {
        5
    }
}

fn price_from_screen(ctx: &PriceAxisContext, y_screen: f64, y_zoom: f64, pan_y: f64) -> f64 {
    let y_norm = 0.5 + (y_screen - 0.5 - pan_y) / y_zoom;
    ctx.hi - y_norm * ctx.span
}

fn screen_from_price(ctx: &PriceAxisContext, price: f64, y_zoom: f64, pan_y: f64) -> f64 {
    let y_norm = (ctx.hi - price) / ctx.span;
    0.5 + (y_norm - 0.5) * y_zoom + pan_y
}

fn time_axis_context(candles: &[Candle]) -> Option<TimeAxisContext> {
    let ts: Vec<u64> = candles
        .iter()
        .filter_map(|c| parse_unix_ts(&c.ts))
        .collect();
    if ts.len() != candles.len() || ts.is_empty() {
        return None;
    }
    Some(TimeAxisContext { ts })
}

fn ts_from_screen(ctx: &TimeAxisContext, x_screen: f64, x_zoom: f64, pan_x: f64) -> u64 {
    let n = ctx.ts.len();
    if n == 1 {
        return ctx.ts[0];
    }
    let x_to_idx = 0.5 + (x_screen - 0.5 - pan_x) / x_zoom;
    let idx = x_to_idx * n as f64 - 0.5;
    let i = idx.clamp(0.0, (n - 1) as f64);
    let lo_i = i.floor() as usize;
    let hi_i = i.ceil() as usize;
    if lo_i == hi_i {
        return ctx.ts[lo_i];
    }
    let t_lo = ctx.ts[lo_i] as f64;
    let t_hi = ctx.ts[hi_i] as f64;
    let frac = i - lo_i as f64;
    (t_lo + (t_hi - t_lo) * frac) as u64
}

fn screen_from_ts(ctx: &TimeAxisContext, ts_unix: u64, x_zoom: f64, pan_x: f64) -> f64 {
    let n = ctx.ts.len();
    if n == 0 {
        return 0.5;
    }
    if n == 1 {
        return 0.5 + pan_x;
    }

    let ts_first = ctx.ts[0];
    let ts_last = ctx.ts[n - 1];
    if ts_first == ts_last {
        return 0.5 + pan_x;
    }

    let mut idx = match ctx.ts.binary_search(&ts_unix) {
        Ok(i) => i as f64,
        Err(i) => {
            if i == 0 {
                0.0
            } else if i >= n {
                (n - 1) as f64
            } else {
                let lo_i = i - 1;
                let hi_i = i;
                let t_lo = ctx.ts[lo_i] as f64;
                let t_hi = ctx.ts[hi_i] as f64;
                let frac = if t_hi > t_lo {
                    (ts_unix as f64 - t_lo) / (t_hi - t_lo)
                } else {
                    0.0
                };
                lo_i as f64 + frac.clamp(0.0, 1.0)
            }
        }
    };

    idx = idx.clamp(0.0, (n - 1) as f64);
    let x_norm = if n > 1 {
        idx / (n as f64 - 1.0)
    } else {
        0.5
    };
    0.5 + (x_norm - 0.5) * x_zoom + pan_x
}

fn build_price_ticks_visible(
    ctx: &PriceAxisContext,
    y_zoom: f64,
    pan_y: f64,
    unit: &str,
) -> Vec<AxisTick> {
    let mut out = Vec::new();
    let steps = 9;
    for i in 0..steps {
        let frac = i as f64 / (steps - 1) as f64;
        let price = price_from_screen(ctx, frac, y_zoom, pan_y);
        let label = format_num_compact(price, ctx.decimals);
        out.push(AxisTick {
            pos: frac as f32,
            label: format!("{label} {unit}").into(),
        });
    }
    out
}

fn build_time_ticks_visible(
    ctx: &TimeAxisContext,
    x_zoom: f64,
    pan_x: f64,
) -> Vec<AxisTick> {
    let mut out = Vec::new();
    let steps = 7;
    for i in 0..steps {
        let frac = i as f64 / (steps - 1) as f64;
        let ts_val = ts_from_screen(ctx, frac, x_zoom, pan_x);
        let label = format_utc(ts_val).unwrap_or_default();
        out.push(AxisTick {
            pos: frac as f32,
            label: label.into(),
        });
    }
    out
}

fn heat_color(intensity: f32) -> slint::Color {
    let t = intensity.clamp(0.0, 1.0) as f64;
    let (cr, cg, cb) = (26.0, 90.0, 190.0);
    let (hr, hg, hb) = (220.0, 60.0, 45.0);
    let r = cr + (hr - cr) * t;
    let g = cg + (hg - cg) * t;
    let b = cb + (hb - cb) * t;
    let a = 0.12 + 0.45 * t;
    slint::Color::from_argb_u8(
        (a * 255.0).round().clamp(10.0, 200.0) as u8,
        r.round().clamp(0.0, 255.0) as u8,
        g.round().clamp(0.0, 255.0) as u8,
        b.round().clamp(0.0, 255.0) as u8,
    )
}

fn build_heatmap_cells(
    state: &AppState,
    price_ctx: Option<&PriceAxisContext>,
    time_ctx: Option<&TimeAxisContext>,
    x_zoom: f64,
    pan_x: f64,
    y_zoom: f64,
    pan_y: f64,
) -> Vec<HeatmapCell> {
    let (Some(pctx), Some(tctx)) = (price_ctx, time_ctx) else {
        return Vec::new();
    };
    if state.heatmap_snapshots.is_empty() {
        return Vec::new();
    }

    let ts_min = *tctx.ts.first().unwrap_or(&0);
    let ts_max = *tctx.ts.last().unwrap_or(&0);
    let mut max_size = 0.0_f64;
    for snap in state.heatmap_snapshots.iter() {
        if snap.ticker != state.current_ticker {
            continue;
        }
        if snap.ts_unix < ts_min || snap.ts_unix > ts_max {
            continue;
        }
        for lvl in &snap.levels {
            max_size = max_size.max(lvl.size);
        }
    }
    if max_size <= 0.0 {
        return Vec::new();
    }

    let n = tctx.ts.len().max(2) as f64;
    let step_norm = 1.0 / (n - 1.0);
    let cell_w = (step_norm * x_zoom).clamp(0.002, 0.04) as f32;
    let cell_h = (0.008 * y_zoom).clamp(0.003, 0.03) as f32;

    let mut out = Vec::new();
    for snap in state.heatmap_snapshots.iter() {
        if snap.ticker != state.current_ticker {
            continue;
        }
        if snap.ts_unix < ts_min || snap.ts_unix > ts_max {
            continue;
        }
        let x = screen_from_ts(tctx, snap.ts_unix, x_zoom, pan_x);
        if x < -0.1 || x > 1.1 {
            continue;
        }
        for lvl in &snap.levels {
            let y = screen_from_price(pctx, lvl.price, y_zoom, pan_y);
            if y < -0.1 || y > 1.1 {
                continue;
            }
            let intensity = (lvl.size / max_size).clamp(0.0, 1.0) as f32;
            out.push(HeatmapCell {
                x: x as f32,
                y: y as f32,
                w: cell_w,
                h: cell_h,
                color: heat_color(intensity),
            });
        }
    }
    out
}

fn build_draw_shape(
    shape: &super::state::DrawShapeState,
    is_preview: bool,
    selected: bool,
    price_ctx: Option<&PriceAxisContext>,
    time_ctx: Option<&TimeAxisContext>,
    x_zoom: f64,
    pan_x: f64,
    y_zoom: f64,
    pan_y: f64,
) -> DrawShape {
    let label = if shape.kind == "Ruler" {
        build_ruler_label(shape, price_ctx, time_ctx, x_zoom, pan_x, y_zoom, pan_y)
    } else {
        String::new()
    };
    let commands = if shape.kind == "Poly" {
        build_poly_commands(shape)
    } else if shape.kind == "Pencil" {
        build_pencil_commands(shape)
    } else {
        String::new()
    };
    DrawShape {
        id: shape.id as i32,
        kind: SharedString::from(shape.kind.clone()),
        x1: shape.x1,
        y1: shape.y1,
        x2: shape.x2,
        y2: shape.y2,
        commands: SharedString::from(commands),
        label: SharedString::from(label),
        is_preview,
        selected,
    }
}

fn build_ruler_label(
    shape: &super::state::DrawShapeState,
    price_ctx: Option<&PriceAxisContext>,
    _time_ctx: Option<&TimeAxisContext>,
    _x_zoom: f64,
    _pan_x: f64,
    y_zoom: f64,
    pan_y: f64,
) -> String {
    let Some(ctx) = price_ctx else {
        return String::new();
    };
    let p1 = price_from_screen(ctx, shape.y1 as f64, y_zoom, pan_y);
    let p2 = price_from_screen(ctx, shape.y2 as f64, y_zoom, pan_y);
    if !p1.is_finite() || !p2.is_finite() || p1 <= 0.0 {
        return String::new();
    }
    let delta = p2 - p1;
    let pct = (delta / p1) * 100.0;
    let direction = if delta >= 0.0 { "Long" } else { "Short" };
    format!(
        "{direction} PnL {pct:+.2}%  Delta {delta:+.*}",
        ctx.decimals,
        direction = direction,
        pct = pct,
        delta = delta
    )
}

fn build_ruler_ticks(
    shape: &super::state::DrawShapeState,
    is_preview: bool,
    price_ctx: Option<&PriceAxisContext>,
    y_zoom: f64,
    pan_y: f64,
) -> Vec<DrawTick> {
    let Some(ctx) = price_ctx else {
        return Vec::new();
    };
    let dx = shape.x2 - shape.x1;
    let dy = shape.y2 - shape.y1;
    let len = ((dx * dx + dy * dy) as f64).sqrt().max(1e-6);
    let mut steps = (len * 12.0).round() as usize;
    steps = steps.clamp(4, 10);

    let mut out = Vec::with_capacity(steps + 1);
    for i in 0..=steps {
        let frac = i as f32 / steps as f32;
        let x = shape.x1 + dx * frac;
        let y = shape.y1 + dy * frac;
        let price = price_from_screen(ctx, y as f64, y_zoom, pan_y);
        let label = if price.is_finite() {
            format_num_compact(price, ctx.decimals)
        } else {
            String::new()
        };
        out.push(DrawTick {
            x,
            y,
            label: label.into(),
            is_preview,
        });
    }
    out
}

fn build_poly_commands(shape: &super::state::DrawShapeState) -> String {
    let mut sides = shape.sides as usize;
    if sides < 3 {
        sides = 5;
    }
    let cx = (shape.x1 + shape.x2) * 0.5;
    let cy = (shape.y1 + shape.y2) * 0.5;
    let rx = (shape.x2 - shape.x1).abs() * 0.5;
    let ry = (shape.y2 - shape.y1).abs() * 0.5;
    if rx <= 0.0 || ry <= 0.0 {
        return String::new();
    }

    let mut out = String::new();
    let tau = std::f64::consts::PI * 2.0;
    for i in 0..sides {
        let theta = (tau / sides as f64) * i as f64 - std::f64::consts::FRAC_PI_2;
        let x = (cx as f64 + rx as f64 * theta.cos()).clamp(0.0, 1.0);
        let y = (cy as f64 + ry as f64 * theta.sin()).clamp(0.0, 1.0);
        if i == 0 {
            out.push_str(&format!("M {:.4} {:.4}", x, y));
        } else {
            out.push_str(&format!(" L {:.4} {:.4}", x, y));
        }
    }
    out.push_str(" Z");
    out
}

fn build_pencil_commands(shape: &super::state::DrawShapeState) -> String {
    if shape.points.len() < 2 {
        return String::new();
    }
    let mut out = String::new();
    for (i, p) in shape.points.iter().enumerate() {
        let x = (p.x as f64).clamp(0.0, 1.0);
        let y = (p.y as f64).clamp(0.0, 1.0);
        if i == 0 {
            out.push_str(&format!("M {:.4} {:.4}", x, y));
        } else {
            out.push_str(&format!(" L {:.4} {:.4}", x, y));
        }
    }
    out
}

// Render volume profile bars
fn render_volume_profile(state: &AppState, ui: &crate::AppWindow) {
    use crate::VolumeProfileBar;

    if state.volume_profile.is_empty() {
        ui.set_volume_profile_bars(ModelRc::new(VecModel::from(Vec::<VolumeProfileBar>::new())));
        return;
    }

    // Find price range for normalization
    let min_price = state.volume_profile.iter().map(|l| l.price).fold(f64::INFINITY, f64::min);
    let max_price = state.volume_profile.iter().map(|l| l.price).fold(f64::NEG_INFINITY, f64::max);
    let price_range = max_price - min_price;

    let bars: Vec<VolumeProfileBar> = state
        .volume_profile
        .iter()
        .map(|level| {
            let norm_price = if price_range > 0.0 {
                ((level.price - min_price) / price_range) as f32
            } else {
                0.5
            };
            VolumeProfileBar {
                norm_price,
                buy_volume: level.buy_volume as f32,
                sell_volume: level.sell_volume as f32,
            }
        })
        .collect();

    ui.set_volume_profile_bars(ModelRc::new(VecModel::from(bars)));
}

// Render liquidity history
fn render_liquidity_history(state: &AppState, ui: &crate::AppWindow) {
    use crate::LiquidityPoint;

    let points: Vec<LiquidityPoint> = state
        .liquidity_history
        .iter()
        .map(|pt| LiquidityPoint {
            ts_unix: pt.ts_unix as i32,
            bid_liq: pt.bid_liq as f32,
            ask_liq: pt.ask_liq as f32,
        })
        .collect();

    ui.set_liquidity_points(ModelRc::new(VecModel::from(points)));
}

// Render imbalance history
fn render_imbalance_history(state: &AppState, ui: &crate::AppWindow) {
    use crate::ImbalancePoint;

    let points: Vec<ImbalancePoint> = state
        .imbalance_history
        .iter()
        .map(|pt| ImbalancePoint {
            ts_unix: pt.ts_unix as i32,
            imbalance: pt.imbalance as f32,
        })
        .collect();

    ui.set_imbalance_points(ModelRc::new(VecModel::from(points)));
}

// Render spread history
fn render_spread_history(state: &AppState, ui: &crate::AppWindow) {
    use crate::SpreadPoint;

    let points: Vec<SpreadPoint> = state
        .spread_history
        .iter()
        .map(|pt| SpreadPoint {
            ts_unix: pt.ts_unix as i32,
            spread_abs: pt.spread as f32,
            spread_bps: pt.spread_bps as f32,
        })
        .collect();

    ui.set_spread_points(ModelRc::new(VecModel::from(points)));
}
