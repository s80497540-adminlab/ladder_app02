// src/bin/main_3.rs

mod candle_agg;

use std::{
    io,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
use candle_agg::{Candle, CandleAgg};
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use rand::{thread_rng, Rng};
use ratatui::{
    backend::CrosstermBackend,
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget},
    Terminal,
};


/// ----- CandleChart: ASCII candlesticks -----
struct CandleChart<'a> {
    candles: &'a [Candle],
    y_min: f64,
    y_max: f64,
}

impl<'a> CandleChart<'a> {
    pub fn new(candles: &'a [Candle], y_min: f64, y_max: f64) -> Self {
        Self { candles, y_min, y_max }
    }
}

impl<'a> Widget for CandleChart<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if self.candles.is_empty() || area.width == 0 || area.height == 0 {
            return;
        }

        let height = area.height as i32;
        let width = area.width as usize;
        let n = self.candles.len().min(width);
        let start = self.candles.len().saturating_sub(n);

        let y_min = self.y_min;
        let y_max = self.y_max;
        let span = (y_max - y_min).max(1e-6);

        let map_price_to_row = |price: f64| -> i32 {
            let ratio = ((price - y_min) / span).clamp(0.0, 1.0);
            let rel = (ratio * (height as f64 - 1.0)).round() as i32;
            (area.y as i32 + (height - 1)) - rel
        };

        for (i, c) in self.candles[start..].iter().enumerate() {
            let x = area.x + i as u16;
            let low_row = map_price_to_row(c.low);
            let high_row = map_price_to_row(c.high);
            let open_row = map_price_to_row(c.open);
            let close_row = map_price_to_row(c.close);

            let color = if c.close >= c.open { Color::Green } else { Color::Red };

            let row_min = area.y as i32;
            let row_max = area.y as i32 + area.height as i32 - 1;

            // wick
            let wick_start = low_row.min(high_row).max(row_min);
            let wick_end = low_row.max(high_row).min(row_max);
            for y in wick_start..=wick_end {
                buf.get_mut(x, y as u16)
                    .set_symbol("│")
                    .set_fg(color);
            }

            // body
            let body_start = open_row.min(close_row).max(row_min);
            let body_end = open_row.max(close_row).min(row_max);

            for y in body_start..=body_end {
                buf.get_mut(x, y as u16)
                    .set_symbol("█")
                    .set_fg(color);
            }
        }
    }
}


/// ----- MAIN -----
fn main() -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Real timeframe aggregators
    let mut tf_30s = CandleAgg::new(30);
    let mut tf_1m = CandleAgg::new(60);
    let mut tf_3m = CandleAgg::new(180);
    let mut tf_5m = CandleAgg::new(300);

    let mut last_price = 3000.0;

    let mut selected_tf: u64 = 60; // default = 1m


    loop {
        // ----- input -----
        if event::poll(Duration::from_millis(1))? {
            if let Event::Key(k) = event::read()? {
                match k.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Char('1') => selected_tf = 30,
                    KeyCode::Char('2') => selected_tf = 60,
                    KeyCode::Char('3') => selected_tf = 180,
                    KeyCode::Char('4') => selected_tf = 300,
                    _ => {}
                }
            }
        }

        // ----- REAL TIME -----
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // ----- generate random walk tick -----
        let mut rng = thread_rng();
        let step: f64 = rng.gen_range(-2.0..2.0);
        last_price += step;
        last_price = last_price.clamp(2950.0, 3050.0);

        // ----- feed aggregators using REAL timestamp -----
        tf_30s.update(ts, last_price);
        tf_1m.update(ts, last_price);
        tf_3m.update(ts, last_price);
        tf_5m.update(ts, last_price);


        // ----- get series -----
        let series = match selected_tf {
            30 => tf_30s.get_series(),
            60 => tf_1m.get_series(),
            180 => tf_3m.get_series(),
            300 => tf_5m.get_series(),
            _ => tf_1m.get_series(),
        };


        // ----- render -----
        terminal.draw(|f| {
            let area = f.area();

            let layout = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(75), Constraint::Percentage(25)])
                .split(area);

            let chart_area = layout[0];
            let info_area = layout[1];

            let tf_label = match selected_tf {
                30 => "30s",
                60 => "1m",
                180 => "3m",
                300 => "5m",
                _ => "1m",
            };

            let block = Block::default()
                .title(Span::styled(
                    format!(
                        " Candles (REAL TIME) — TF: {}  (1=30s, 2=1m, 3=3m, 4=5m, q=quit) ",
                        tf_label
                    ),
                    Style::default().add_modifier(Modifier::BOLD),
                ))
                .borders(Borders::ALL);

            let inner = block.inner(chart_area);
            f.render_widget(block, chart_area);

            if !series.is_empty() && inner.width > 0 && inner.height > 0 {
                let window_len = inner.width as usize;

                let visible: Vec<Candle> = series
                    .iter()
                    .rev()
                    .take(window_len)
                    .cloned()
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();

                let chart = CandleChart::new(&visible, 2950.0, 3050.0);
                f.render_widget(chart, inner);
            }

            // INFO PANEL
            let mut info_lines = Vec::new();

            if let Some(c) = series.last() {
                info_lines.push(Line::from(format!("Bucket start (unix sec): {}", c.t)));
                info_lines.push(Line::from(format!("Open  : {:.2}", c.open)));
                info_lines.push(Line::from(format!("High  : {:.2}", c.high)));
                info_lines.push(Line::from(format!("Low   : {:.2}", c.low)));
                info_lines.push(Line::from(format!("Close : {:.2}", c.close)));
            }

            info_lines.push(Line::from(""));
            info_lines.push(Line::from("Candles now track actual wall-clock time."));
            info_lines.push(Line::from("Try switching TF and wait to see real closes."));

            let info = Paragraph::new(info_lines).block(
                Block::default().title(" Info ").borders(Borders::ALL),
            );

            f.render_widget(info, info_area);
        })?;

        // smooth rendering, timing does NOT affect candlesticks
        std::thread::sleep(Duration::from_millis(33));
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}
