use crossbeam_channel::{unbounded, Receiver};
use egui::{CentralPanel, Context, TopBottomPanel};
use egui_plot::{Line, Plot, PlotPoints};
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
    // background data from a worker
    rx: Receiver<f64>,
    latest_val: f64,
    series: Vec<[f64; 2]>, // (t, value) for plotting
    // simple log
    log: Vec<String>,
}

impl MyApp {
    fn new() -> Self {
        let (tx, rx) = unbounded::<f64>();

        // Spawn a lightweight background producer you can replace later
        thread::spawn(move || {
            let mut t = 0.0_f64;
            loop {
                // pretend this is an incoming "price"/metric
                let val = (t * 0.2).sin() * 10.0 + 100.0;
                let _ = tx.send(val);
                t += 1.0;
                thread::sleep(Duration::from_millis(50)); // ~20 Hz feed
            }
        });

        Self {
            counter: 0,
            start: Instant::now(),
            rx,
            latest_val: 0.0,
            series: Vec::with_capacity(10_000),
            log: vec!["egui started".into()],
        }
    }

    fn poll_background(&mut self) {
        // Drain any pending values quickly (non-blocking)
        for v in self.rx.try_iter() {
            self.latest_val = v;
            let t_sec = self.start.elapsed().as_secs_f64();
            self.series.push([t_sec, v]);
            // keep the series bounded
            if self.series.len() > 2_000 {
                self.series.drain(0..1_000);
            }
        }
    }
}

impl eframe::App for MyApp {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        // target ~60 FPS; adjust to 20ms for ~50 FPS if you prefer
        ctx.request_repaint_after(Duration::from_millis(50));

        // pull background messages
        self.poll_background();

        TopBottomPanel::top("top_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Increment").clicked() {
                    self.counter += 1;
                    self.log.push(format!("counter -> {}", self.counter));
                }
                ui.label(format!("counter: {}", self.counter));
                ui.separator();
                ui.label(format!("latest: {:.4}", self.latest_val));
                ui.separator();
                ui.label(format!("fps target: ~60"));
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
                .view_aspect(2.0) // wider than tall
                .show(ui, |plot_ui| {
                    plot_ui.line(Line::new(points).name("bg value"));
                });
            
            ui.separator();
            ui.heading("Log");
            egui::ScrollArea::vertical().max_height(160.0).show(ui, |ui| {
                for line in self.log.iter().rev().take(200) {
                    ui.label(line);
                }
            });
        });
    }
}