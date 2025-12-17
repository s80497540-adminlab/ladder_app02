use crossbeam_channel::{unbounded, Receiver};
use egui::{CentralPanel, Context, TopBottomPanel};
use egui_plot::{Line, Plot, PlotPoints};
use rand::{distributions::Uniform, Rng};
use std::collections::VecDeque;
use std::thread;
use std::time::{Duration, Instant};

fn main() -> eframe::Result<()> {
    let native_options = eframe::NativeOptions::default();
    eframe::run_native(
        "egui sandbox",
        native_options,
        Box::new(|_cc| Box::new(MyApp::new())),
    )
}

struct MyApp {
    // demo state
    counter: u64,
    start: Instant,

    // numeric feed (for the plot)
    rx_vals: Receiver<f64>,
    latest_val: f64,
    series: Vec<[f64; 2]>,

    // json-like messages
    rx_msgs: Receiver<String>,
    messages: VecDeque<String>, // ring buffer
    max_msgs: usize,
}

impl MyApp {
    fn new() -> Self {
        let (tx_vals, rx_vals) = unbounded::<f64>();
        let (tx_msgs, rx_msgs) = unbounded::<String>();

        // Background numeric producer (~20 Hz)
        thread::spawn(move || {
            let mut t = 0.0_f64;
            loop {
                let val = (t * 0.2).sin() * 10.0 + 100.0;
                let _ = tx_vals.send(val);
                t += 1.0;
                thread::sleep(Duration::from_millis(50));
            }
        });

        // Background JSON message producer (~3–5 msgs/sec)
        thread::spawn(move || {
            let mut rng = rand::thread_rng();
            let side = ["buy", "sell"];
            let sym = ["ETH-USD", "BTC-USD", "SOL-USD"];
            let qty_d = Uniform::new(0.01_f64, 3.0);
            let px_d = Uniform::new(50.0_f64, 500.0);
            let mut id: u64 = 1;

            loop {
                let msg = serde_json::json!({
                    "id": id,
                    "ts_ms": now_ms(),
                    "symbol": sym[rng.gen_range(0..sym.len())],
                    "side": side[rng.gen_range(0..side.len())],
                    "qty": (rng.sample(qty_d) * 100.0).round() / 100.0,
                    "price": (rng.sample(px_d) * 100.0).round() / 100.0
                })
                .to_string();

                let _ = tx_msgs.send(msg);
                id += 1;

                // random-ish cadence: 200–400 ms
                let sleep_ms: u64 = rng.gen_range(200..=400);
                thread::sleep(Duration::from_millis(sleep_ms));
            }
        });

        Self {
            counter: 0,
            start: Instant::now(),
            rx_vals,
            latest_val: 0.0,
            series: Vec::with_capacity(10_000),

            rx_msgs,
            messages: VecDeque::with_capacity(2_000),
            max_msgs: 1_000,
        }
    }

    fn poll_background(&mut self) {
        // Drain numeric values (non-blocking)
        for v in self.rx_vals.try_iter() {
            self.latest_val = v;
            let t_sec = self.start.elapsed().as_secs_f64();
            self.series.push([t_sec, v]);
            if self.series.len() > 2_000 {
                self.series.drain(0..1_000);
            }
        }

        // Drain JSON messages (non-blocking)
        for m in self.rx_msgs.try_iter() {
            if self.messages.len() >= self.max_msgs {
                self.messages.pop_front();
            }
            self.messages.push_back(m);
        }
    }
}

impl eframe::App for MyApp {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(16));
        self.poll_background();

        TopBottomPanel::top("top_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Increment").clicked() {
                    self.counter += 1;
                }
                ui.label(format!("counter: {}", self.counter));
                ui.separator();
                ui.label(format!("latest: {:.4}", self.latest_val));
                ui.separator();
                ui.label("fps target: ~60");
            });
        });

        CentralPanel::default().show(ctx, |ui| {
            ui.heading("Quick sanity plot");
            let points: PlotPoints = self
                .series
                .iter()
                .map(|[t, v]| [*t, *v])
                .collect::<Vec<_>>()
                .into();

            Plot::new("plot")
                .allow_boxed_zoom(false)
                .view_aspect(2.0)
                .show(ui, |plot_ui| {
                    plot_ui.line(Line::new(points).name("bg value"));
                });

            ui.separator();
            ui.heading("Messages (JSON-like)");
            egui::ScrollArea::vertical().max_height(240.0).show(ui, |ui| {
                // Show newest last (natural reading order)
                for msg in self.messages.iter() {
                    ui.monospace(msg);
                }
            });
        });
    }
}

// simple monotonic-ish ms timestamp; you can switch to SystemTime if preferred
fn now_ms() -> u128 {
    use std::time::SystemTime;
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_millis()
}
