use super::state::*;
use crate::{BookLevel, Receipt, Trade};
use slint::{ModelRc, SharedString, VecModel};

pub fn render(state: &AppState, ui: &crate::AppWindow) {
    // Core
    ui.set_current_ticker(SharedString::from(state.current_ticker.clone()));
    ui.set_mode(SharedString::from(state.mode.clone()));
    ui.set_time_mode(SharedString::from(state.time_mode.clone()));

    ui.set_candle_tf_secs(state.candle_tf_secs);
    ui.set_candle_window_minutes(state.candle_window_minutes);
    ui.set_dom_depth_levels(state.dom_depth_levels);

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
