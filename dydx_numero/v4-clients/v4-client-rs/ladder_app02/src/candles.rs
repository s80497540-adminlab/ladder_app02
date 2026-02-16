use std::collections::VecDeque;

#[derive(Debug, Clone)]
pub struct Candle {
    pub start_ms: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

#[derive(Debug)]
pub struct CandleAgg {
    tf_secs: i64,
    window_minutes: i64,
    candles: VecDeque<Candle>,
    cur: Option<Candle>,
}

impl CandleAgg {
    pub fn new(tf_secs: i64, window_minutes: i64) -> Self {
        Self {
            tf_secs: tf_secs.max(1),
            window_minutes: window_minutes.max(1),
            candles: VecDeque::new(),
            cur: None,
        }
    }

    pub fn set_tf(&mut self, tf_secs: i64) {
        self.tf_secs = tf_secs.max(1);
        self.candles.clear();
        self.cur = None;
    }

    pub fn set_window_minutes(&mut self, window_minutes: i64) {
        self.window_minutes = window_minutes.max(1);
        self.trim_to_window();
    }

    fn bucket_start_ms(&self, ts_ms: i64) -> i64 {
        let tf_ms = self.tf_secs * 1000;
        (ts_ms / tf_ms) * tf_ms
    }

    fn trim_to_window(&mut self) {
        let max = (self.window_minutes * 60 / self.tf_secs).max(10) as usize;
        while self.candles.len() > max {
            self.candles.pop_front();
        }
    }

    /// Feed one price sample (trade price is best; mid-price also works).
    pub fn on_tick(&mut self, ts_ms: i64, price: f64, vol: f64) -> bool {
        let start = self.bucket_start_ms(ts_ms);

        match self.cur.as_mut() {
            None => {
                self.cur = Some(Candle {
                    start_ms: start,
                    open: price,
                    high: price,
                    low: price,
                    close: price,
                    volume: vol,
                });
                true
            }
            Some(c) if c.start_ms == start => {
                c.high = c.high.max(price);
                c.low = c.low.min(price);
                c.close = price;
                c.volume += vol;
                true
            }
            Some(prev) => {
                // finalize previous
                let finished = std::mem::replace(prev, Candle {
                    start_ms: start,
                    open: price,
                    high: price,
                    low: price,
                    close: price,
                    volume: vol,
                });
                self.candles.push_back(finished);
                self.trim_to_window();
                true
            }
        }
    }

    pub fn snapshot(&self) -> Vec<Candle> {
        let mut out: Vec<Candle> = self.candles.iter().cloned().collect();
        if let Some(cur) = &self.cur {
            out.push(cur.clone());
        }
        out
    }
}
