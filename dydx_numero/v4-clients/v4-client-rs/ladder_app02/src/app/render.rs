use super::state::*;
use crate::{AxisTick, BookLevel, CandlePoint, CandleRow, Receipt, Trade};
use chrono::{DateTime, NaiveDateTime, Utc};
use slint::{ModelRc, SharedString, VecModel};

const MAX_CONDENSED_POINTS: usize = 600;

pub fn render(state: &AppState, ui: &crate::AppWindow) {
    // Core
    ui.set_current_ticker(SharedString::from(state.current_ticker.clone()));
    ui.set_mode(SharedString::from(state.mode.clone()));
    ui.set_time_mode(SharedString::from(state.time_mode.clone()));

    ui.set_candle_tf_secs(state.candle_tf_secs);
    ui.set_candle_window_minutes(state.candle_window_minutes);
    ui.set_dom_depth_levels(state.dom_depth_levels);
    ui.set_render_all_candles(state.render_all_candles);

    ui.set_trade_side(SharedString::from(state.trade_side.clone()));
    ui.set_trade_size(state.trade_size);
    ui.set_trade_leverage(state.trade_leverage);

    ui.set_trade_real_mode(state.trade_real_mode);
    ui.set_trade_real_armed(state.trade_real_armed);
    ui.set_trade_real_arm_phrase(SharedString::from(state.trade_real_arm_phrase.clone()));
    ui.set_trade_real_arm_status(SharedString::from(state.trade_real_arm_status.clone()));

    ui.set_balance_usdc(state.balance_usdc);
    ui.set_balance_pnl(state.balance_pnl);

    ui.set_current_time(SharedString::from(state.current_time.clone()));
    ui.set_order_message(SharedString::from(state.order_message.clone()));
    ui.set_axis_price_unit(SharedString::from(state.current_ticker.clone()));
    ui.set_candle_feed_status(SharedString::from(state.candle_feed_status()));
    ui.set_history_status(SharedString::from(state.history_status()));

    // Metrics
    ui.set_mid_price(state.metrics.mid as f32);
    ui.set_best_bid(state.metrics.best_bid as f32);
    ui.set_best_ask(state.metrics.best_ask as f32);
    ui.set_spread(state.metrics.spread as f32);
    ui.set_imbalance(state.metrics.imbalance as f32);

    // Book models
    let bids: Vec<BookLevel> = state
        .bids
        .iter()
        .map(|b| BookLevel {
            price: SharedString::from(b.price.clone()),
            size: SharedString::from(b.size.clone()),
            depth_ratio: b.depth_ratio,
            is_best: b.is_best,
        })
        .collect();
    ui.set_bids(ModelRc::new(VecModel::from(bids)));

    let asks: Vec<BookLevel> = state
        .asks
        .iter()
        .map(|a| BookLevel {
            price: SharedString::from(a.price.clone()),
            size: SharedString::from(a.size.clone()),
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
            open: SharedString::from(format!("{:.2}", c.open)),
            high: SharedString::from(format!("{:.2}", c.high)),
            low: SharedString::from(format!("{:.2}", c.low)),
            close: SharedString::from(format!("{:.2}", c.close)),
            volume: SharedString::from(format!("{:.2}", c.volume)),
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

    // Axis ticks derived from visible window (pan/zoom aware)
    let price_ticks = build_price_ticks_visible(
        candles_for_view,
        ui.get_chart_y_zoom() as f64,
        ui.get_chart_pan_y() as f64,
        &state.current_ticker,
    );
    ui.set_price_ticks(ModelRc::new(VecModel::from(price_ticks)));

    let time_ticks = build_time_ticks_visible(
        candles_for_view,
        ui.get_chart_x_zoom() as f64,
        ui.get_chart_pan_x() as f64,
    );
    ui.set_time_ticks(ModelRc::new(VecModel::from(time_ticks)));

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

fn parse_unix_ts(ts: &str) -> Option<u64> {
    ts.strip_prefix("unix:")?.parse().ok()
}

fn format_utc(ts_unix: u64) -> Option<String> {
    let dt = NaiveDateTime::from_timestamp_opt(ts_unix as i64, 0)?;
    let dt: DateTime<Utc> = DateTime::from_naive_utc_and_offset(dt, Utc);
    Some(dt.format("%H:%M:%S UTC").to_string())
}

fn build_price_ticks_visible(
    candles: &[Candle],
    y_zoom: f64,
    pan_y: f64,
    unit: &str,
) -> Vec<AxisTick> {
    let mut out = Vec::new();
    if candles.is_empty() {
        return out;
    }

    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for c in candles {
        lo = lo.min(c.low);
        hi = hi.max(c.high);
    }
    if !lo.is_finite() || !hi.is_finite() || hi <= lo {
        return out;
    }
    let span = hi - lo;

    // invert screen y (0..1) to price using current pan/zoom
    let y_to_price = |y_screen: f64| {
        let y_norm = 0.5 + (y_screen - 0.5 - pan_y) / y_zoom;
        hi - y_norm * span
    };

    let steps = 9;
    for i in 0..steps {
        let frac = i as f64 / (steps - 1) as f64;
        let price = y_to_price(frac);
        let decimals = if span >= 1000.0 {
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
        };
        out.push(AxisTick {
            pos: frac as f32,
            label: format!("{price:.decimals$} {unit}").into(),
        });
    }
    out
}

fn build_time_ticks_visible(
    candles: &[Candle],
    x_zoom: f64,
    pan_x: f64,
) -> Vec<AxisTick> {
    let mut out = Vec::new();
    let n = candles.len();
    if n == 0 {
        return out;
    }

    let ts: Vec<u64> = candles
        .iter()
        .filter_map(|c| parse_unix_ts(&c.ts))
        .collect();
    if ts.len() != n {
        return out;
    }

    let idx_to_ts = |idx: f64| -> u64 {
        let i = idx.clamp(0.0, (n - 1) as f64);
        let lo_i = i.floor() as usize;
        let hi_i = i.ceil() as usize;
        if lo_i == hi_i {
            return ts[lo_i];
        }
        let t_lo = ts[lo_i] as f64;
        let t_hi = ts[hi_i] as f64;
        let frac = i - lo_i as f64;
        (t_lo + (t_hi - t_lo) * frac) as u64
    };

    // invert screen x (0..1) to candle index using pan/zoom
    let x_to_idx = |x_screen: f64| -> f64 {
        0.5 + (x_screen - 0.5 - pan_x) / x_zoom
    };

    let steps = 7;
    for i in 0..steps {
        let frac = i as f64 / (steps - 1) as f64;
        let idx = x_to_idx(frac) * n as f64 - 0.5;
        let ts_val = idx_to_ts(idx);
        let label = format_utc(ts_val).unwrap_or_default();
        out.push(AxisTick {
            pos: frac as f32,
            label: label.into(),
        });
    }
    out
}
