use crate::app::{AppEvent, FeedEvent};
use std::{thread, time::Duration};

pub fn start_dummy_feed(tx: std::sync::mpsc::Sender<AppEvent>) {
    thread::spawn(move || {
        let mut px = 2500.0f64;
        let mut step = 0.25f64;
        let mut n: u64 = 0;

        loop {
            thread::sleep(Duration::from_millis(200));
            n += 1;

            // tiny deterministic “random walk”
            if n % 17 == 0 {
                step = -step;
            }
            px = (px + step).max(10.0);

            let best_bid = px - 0.5;
            let best_ask = px + 0.5;

            let bid_liq = 120.0 + ((n % 25) as f64);
            let ask_liq = 110.0 + (((n + 7) % 25) as f64);

            let ts = crate::app::state::now_unix();

            let _ = tx.send(AppEvent::Feed(FeedEvent::BookTop {
                ts_unix: ts,
                best_bid,
                best_ask,
                bid_liq,
                ask_liq,
            }));

            // trade every other tick
            if n % 2 == 0 {
                let side = if n % 4 == 0 { "Buy" } else { "Sell" }.to_string();
                let size = format!("{:.4}", 0.01 + ((n % 9) as f64) * 0.005);

                let _ = tx.send(AppEvent::Feed(FeedEvent::Trade {
                    ts_unix: ts,
                    side,
                    size,
                    source: "dummy".to_string(),
                }));
            }
        }
    });
}
