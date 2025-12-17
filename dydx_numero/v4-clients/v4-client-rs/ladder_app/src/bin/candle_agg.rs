// ladder_app/src/bin/candle_agg.rs

use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

#[derive(Clone, Copy, Debug)]
pub struct Candle {
    /// Unix timestamp (seconds) of the bucket start
    pub t: u64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

#[derive(Clone, Debug)]
pub struct CandleAgg {
    tf_secs: u64,
    series: Vec<Candle>,
}

impl CandleAgg {
    pub fn new(tf_secs: u64) -> Self {
        Self {
            tf_secs,
            series: Vec::new(),
        }
    }

    pub fn tf(&self) -> u64 {
        self.tf_secs
    }

    /// Update with a tick (ts, price, volume)
    pub fn update(&mut self, ts: u64, price: f64, volume: f64) {
        let bucket_start = (ts / self.tf_secs) * self.tf_secs;

        if let Some(last) = self.series.last_mut() {
            if last.t == bucket_start {
                // update current candle
                if price > last.high {
                    last.high = price;
                }
                if price < last.low {
                    last.low = price;
                }
                last.close = price;
                last.volume += volume;
                return;
            }
        }

        // new candle
        self.series.push(Candle {
            t: bucket_start,
            open: price,
            high: price,
            low: price,
            close: price,
            volume,
        });
    }

    /// Read-only access to internal series
    pub fn series(&self) -> &[Candle] {
        &self.series
    }

    /// Mutable access if you really want to tweak
    pub fn series_mut(&mut self) -> &mut Vec<Candle> {
        &mut self.series
    }

    /// Append a fully-formed historical candle (for loading from disk).
    pub fn push_candle(&mut self, c: Candle) {
        self.series.push(c);
    }

    /// Load candles from a CSV file into this aggregator.
    ///
    /// Format:
    ///   ts,tf_secs,open,high,low,close,volume
    ///
    /// Only lines where tf_secs == self.tf_secs are applied.
    pub fn load_from_csv<P: AsRef<Path>>(&mut self, path: P) {
        let path = path.as_ref();
        if !path.exists() {
            return;
        }

        let file = match File::open(path) {
            Ok(f) => f,
            Err(_) => return,
        };
        let reader = BufReader::new(file);

        for (idx, line) in reader.lines().enumerate() {
            let line = match line {
                Ok(l) => l,
                Err(_) => continue,
            };

            if idx == 0 && line.starts_with("ts,") {
                // header
                continue;
            }

            let parts: Vec<&str> = line.split(',').collect();
            if parts.len() != 7 {
                continue;
            }

            let ts: u64 = match parts[0].parse() {
                Ok(v) => v,
                Err(_) => continue,
            };
            let tf: u64 = match parts[1].parse() {
                Ok(v) => v,
                Err(_) => continue,
            };
            if tf != self.tf_secs {
                continue;
            }

            let open: f64 = parts[2].parse().unwrap_or(0.0);
            let high: f64 = parts[3].parse().unwrap_or(open);
            let low: f64 = parts[4].parse().unwrap_or(open);
            let close: f64 = parts[5].parse().unwrap_or(open);
            let vol: f64 = parts[6].parse().unwrap_or(0.0);

            self.series.push(Candle {
                t: ts,
                open,
                high,
                low,
                close,
                volume: vol,
            });
        }
    }

    /// Save the entire series to CSV.
    /// We overwrite the file each time we flush.
    pub fn save_to_csv<P: AsRef<Path>>(&self, path: P) {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        let mut file = match File::create(path) {
            Ok(f) => f,
            Err(_) => return,
        };

        let _ = writeln!(file, "ts,tf_secs,open,high,low,close,volume");

        for c in &self.series {
            let _ = writeln!(
                file,
                "{},{},{:.8},{:.8},{:.8},{:.8},{:.8}",
                c.t, self.tf_secs, c.open, c.high, c.low, c.close, c.volume
            );
        }
    }
}
