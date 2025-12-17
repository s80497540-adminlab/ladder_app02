// ladder_app02/src/bin/data_daemon02.rs
//
// Synthetic market data daemon for ladder_app02.
//
// - Writes CSV files into ./data:
//
//     data/orderbook_ETH-USD.csv
//     data/orderbook_BTC-USD.csv
//     data/orderbook_SOL-USD.csv
//
//     data/trades_ETH-USD.csv
//     data/trades_BTC-USD.csv
//     data/trades_SOL-USD.csv
//
// - CSV format (orderbook_*):
//     ts,u64,ticker,string,kind,string,side,string,price,f64,size,f64
//     1710000000,ETH-USD,orderbook,bid,3050.25,1.2345
//
// - CSV format (trades_*):
//     ts,u64,ticker,string,source,string,side,string,size_str,string
//     1710000001,ETH-USD,sim,buy,0.01234567
//
// This does NOT talk to dYdX yet. It's just a random-walk simulator.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use std::fs::{create_dir_all, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_secs()
}

#[derive(Clone, Debug)]
struct TickerState {
    name: String,
    mid: f64,
    vol_scale: f64,
}

fn open_append(path: &Path) -> std::io::Result<std::fs::File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
}

fn write_orderbook_snapshot(
    ob_path: &Path,
    ts: u64,
    ticker: &str,
    mid: f64,
    rng: &mut StdRng,
) -> std::io::Result<()> {
    let mut f = open_append(ob_path)?;

    let levels = 10usize;
    let tick = (mid * 0.0005_f64).max(0.01);

    for i in 0..levels {
        let price = mid - (i as f64) * tick;
        let size: f64 = rng.gen_range(0.01..0.5);
        let line = format!(
            "{ts},{ticker},orderbook,bid,{price:.2},{size:.6}\n"
        );
        f.write_all(line.as_bytes())?;
    }

    for i in 0..levels {
        let price = mid + (i as f64) * tick;
        let size: f64 = rng.gen_range(0.01..0.5);
        let line = format!(
            "{ts},{ticker},orderbook,ask,{price:.2},{size:.6}\n"
        );
        f.write_all(line.as_bytes())?;
    }

    Ok(())
}

fn maybe_write_trade(
    tr_path: &Path,
    ts: u64,
    ticker: &str,
    mid: f64,
    rng: &mut StdRng,
) -> std::io::Result<()> {
    let roll: f64 = rng.gen();
    if roll > 0.3 {
        return Ok(());
    }

    let side = if rng.gen::<f64>() < 0.5 { "buy" } else { "sell" };
    let size: f64 = rng.gen_range(0.001..0.05);
    let size_str = format!("{:.8}", size);
    let source = "sim";

    let _price_jitter = rng.gen_range(-0.0005..0.0005) * mid;

    let mut f = open_append(tr_path)?;
    let line = format!(
        "{ts},{ticker},{source},{side},{size_str}\n"
    );
    f.write_all(line.as_bytes())?;

    Ok(())
}

fn main() {
    let base_dir = PathBuf::from("data");
    println!(
        "[data_daemon02] Starting synthetic data daemon. Writing to: {}",
        base_dir.display()
    );

    if let Err(e) = create_dir_all(&base_dir) {
        eprintln!("[data_daemon02] Failed to create data dir: {e}");
        return;
    }

    let mut rng = StdRng::from_entropy();

    let mut tickers = vec![
        TickerState {
            name: "ETH-USD".to_string(),
            mid: 3000.0,
            vol_scale: 0.003,
        },
        TickerState {
            name: "BTC-USD".to_string(),
            mid: 60000.0,
            vol_scale: 0.002,
        },
        TickerState {
            name: "SOL-USD".to_string(),
            mid: 150.0,
            vol_scale: 0.005,
        },
    ];

    loop {
        let ts = now_unix();

        for tk in &mut tickers {
            let step = rng.gen_range(-1.0..1.0) * tk.mid * tk.vol_scale;
            tk.mid = (tk.mid + step).max(1.0);

            let ob_path = base_dir.join(format!("orderbook_{}.csv", tk.name));
            let tr_path = base_dir.join(format!("trades_{}.csv", tk.name));

            if let Err(e) = write_orderbook_snapshot(&ob_path, ts, &tk.name, tk.mid, &mut rng) {
                eprintln!(
                    "[data_daemon02] error writing orderbook for {}: {e}",
                    tk.name
                );
            }

            if let Err(e) = maybe_write_trade(&tr_path, ts, &tk.name, tk.mid, &mut rng) {
                eprintln!(
                    "[data_daemon02] error writing trade for {}: {e}",
                    tk.name
                );
            }
        }

        thread::sleep(Duration::from_millis(200));
    }
}
