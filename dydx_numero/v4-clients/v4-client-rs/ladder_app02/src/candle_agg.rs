use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

#[derive(Clone, Debug, Default)]
pub struct Candle {
    pub t: u64,      // bucket start (unix seconds)
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

#[derive(Clone, Debug)]
pub struct CandleAgg {
    tf_secs: u64,
    cur: Option<Candle>,
    series: Vec<Candle>,
}

impl CandleAgg {
    pub fn new(tf_secs: u64) -> Self {
        Self {
            tf_secs: tf_secs.max(1),
            cur: None,
            series: Vec::new(),
        }
    }

    pub fn tf(&self) -> u64 {
        self.tf_secs
    }

    pub fn series(&self) -> &Vec<Candle> {
        &self.series
    }

    pub fn series_mut(&mut self) -> &mut Vec<Candle> {
        &mut self.series
    }

    fn bucket_start(&self, ts: u64) -> u64 {
        if self.tf_secs == 0 {
            ts
        } else {
            ts - (ts % self.tf_secs)
        }
    }

    fn flush_cur(&mut self) {
        if let Some(c) = self.cur.take() {
            self.series.push(c);
        }
    }

    pub fn update(&mut self, ts: u64, price: f64, volume: f64) {
        let b = self.bucket_start(ts);

        match self.cur.as_mut() {
            None => {
                self.cur = Some(Candle {
                    t: b,
                    open: price,
                    high: price,
                    low: price,
                    close: price,
                    volume: volume.max(0.0),
                });
            }
            Some(c) => {
                if c.t != b {
                    // finalize previous bucket
                    let prev = self.cur.take().unwrap();
                    self.series.push(prev);

                    self.cur = Some(Candle {
                        t: b,
                        open: price,
                        high: price,
                        low: price,
                        close: price,
                        volume: volume.max(0.0),
                    });
                } else {
                    // same bucket
                    if price > c.high {
                        c.high = price;
                    }
                    if price < c.low {
                        c.low = price;
                    }
                    c.close = price;
                    c.volume += volume.max(0.0);
                }
            }
        }
    }

    pub fn load_from_csv(&mut self, path: &Path) {
        self.cur = None;
        self.series.clear();

        if !path.exists() {
            return;
        }
        let Ok(f) = File::open(path) else { return; };
        let reader = BufReader::new(f);

        for line in reader.lines().flatten() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let parts: Vec<&str> = line.split(',').collect();
            if parts.len() < 6 {
                continue;
            }

            let Ok(t) = parts[0].parse::<u64>() else { continue; };
            let Ok(open) = parts[1].parse::<f64>() else { continue; };
            let Ok(high) = parts[2].parse::<f64>() else { continue; };
            let Ok(low) = parts[3].parse::<f64>() else { continue; };
            let Ok(close) = parts[4].parse::<f64>() else { continue; };
            let Ok(volume) = parts[5].parse::<f64>() else { continue; };

            self.series.push(Candle {
                t,
                open,
                high,
                low,
                close,
                volume,
            });
        }

        self.series.sort_by_key(|c| c.t);
    }

    pub fn save_to_csv(&mut self, path: &Path) {
        // include current candle if present
        let had_cur = self.cur.is_some();
        if had_cur {
            self.flush_cur();
        }

        let Ok(mut f) = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)
        else {
            return;
        };

        for c in &self.series {
            let _ = writeln!(
                f,
                "{},{},{},{},{},{}",
                c.t, c.open, c.high, c.low, c.close, c.volume
            );
        }
    }
}
