// ladder_app/src/bin/candle_agg.rs
//
// Simple real-time candle aggregator using fixed bucket seconds.
// API used by gui_app2.rs:
//
//   let mut agg = CandleAgg::new(60);
//   agg.update(ts_unix_secs, price);
//   let series: Vec<Candle> = agg.get_series();

#[derive(Clone, Debug)]
pub struct Candle {
    pub t: u64,      // bucket start unix seconds
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
}

#[derive(Clone, Debug)]
pub struct CandleAgg {
    bucket_secs: u64,
    current: Option<Candle>,
    series: Vec<Candle>,
    max_series: usize,
}

impl CandleAgg {
    pub fn new(bucket_secs: u64) -> Self {
        Self {
            bucket_secs,
            current: None,
            series: Vec::new(),
            max_series: 2000,
        }
    }

    fn bucket_start(&self, ts: u64) -> u64 {
        ts - (ts % self.bucket_secs)
    }

    pub fn update(&mut self, ts: u64, price: f64) {
        let bstart = self.bucket_start(ts);

        match self.current.as_mut() {
            None => {
                self.current = Some(Candle {
                    t: bstart,
                    open: price,
                    high: price,
                    low: price,
                    close: price,
                });
            }
            Some(c) if c.t == bstart => {
                // same bucket, update OHLC
                c.high = c.high.max(price);
                c.low = c.low.min(price);
                c.close = price;
            }
            Some(_) => {
                // new bucket: push old, start new
                if let Some(old) = self.current.take() {
                    self.series.push(old);
                    if self.series.len() > self.max_series {
                        let overflow = self.series.len() - self.max_series;
                        self.series.drain(0..overflow);
                    }
                }

                self.current = Some(Candle {
                    t: bstart,
                    open: price,
                    high: price,
                    low: price,
                    close: price,
                });
            }
        }
    }

    /// Returns completed candles + current in-progress candle at the end.
    pub fn get_series(&self) -> Vec<Candle> {
        let mut out = self.series.clone();
        if let Some(c) = &self.current {
            out.push(c.clone());
        }
        out
    }
}
