// ladder_app/src/bin/candle_agg.rs

#[derive(Clone, Copy, Debug)]
pub struct Candle {
    pub t: u64,      // bucket start timestamp (unix seconds)
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64, // aggregated volume for this bucket
}

#[derive(Clone, Debug)]
pub struct CandleAgg {
    period_secs: u64,
    current: Option<Candle>,
    closed: Vec<Candle>,
    max_keep: usize,
}

impl CandleAgg {
    pub fn new(period_secs: u64) -> Self {
        Self {
            period_secs,
            current: None,
            closed: Vec::new(),
            max_keep: 5000,
        }
    }

    fn bucket_start(&self, ts: u64) -> u64 {
        ts - (ts % self.period_secs)
    }

    /// Update with a new tick: (timestamp, price, volume)
    /// For now volume can be synthetic (e.g. 1.0 per tick), later you can plug real trade size.
    pub fn update(&mut self, ts: u64, price: f64, volume: f64) {
        let bstart = self.bucket_start(ts);

        match &mut self.current {
            None => {
                self.current = Some(Candle {
                    t: bstart,
                    open: price,
                    high: price,
                    low: price,
                    close: price,
                    volume: volume.max(0.0),
                });
            }
            Some(c) => {
                if c.t != bstart {
                    // close previous bucket, push to closed
                    let finished = *c;
                    self.closed.push(finished);
                    if self.closed.len() > self.max_keep {
                        let overflow = self.closed.len() - self.max_keep;
                        self.closed.drain(0..overflow);
                    }

                    // start new bucket
                    *c = Candle {
                        t: bstart,
                        open: price,
                        high: price,
                        low: price,
                        close: price,
                        volume: volume.max(0.0),
                    };
                } else {
                    // same bucket, update O/H/L/C + volume
                    c.close = price;
                    if price > c.high {
                        c.high = price;
                    }
                    if price < c.low {
                        c.low = price;
                    }
                    c.volume += volume.max(0.0);
                }
            }
        }
    }

    /// Return all closed + current as a single time series
    pub fn get_series(&self) -> Vec<Candle> {
        let mut out = self.closed.clone();
        if let Some(c) = self.current {
            out.push(c);
        }
        out
    }
}



