// src/bin/main_6.rs
//
// Multi-chart terminal app:
//
// Tabs:
//   [O] Orderbook + Depth (fake synthetic book)
//   [C] Candles + RSI (real-time timeframes 30s/1m/3m/5m)
//
// Keys:
//   o = orderbook/depth tab
//   c = candles/RSI tab
//   1 = 30s candles
//   2 = 1m candles
//   3 = 3m candles
//   4 = 5m candles
//   q = quit

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
use rand::{rng, Rng};
use ratatui::{
    backend::CrosstermBackend,
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Axis, Block, Borders, Chart, Dataset, Paragraph, Widget},
    Terminal,
};
use ratatui::symbols::Marker;

/// ----- Simple synthetic orderbook structs -----

#[derive(Clone, Default)]
struct SideBook {
    // (price, size)
    levels: Vec<(f64, f64)>,
}

#[derive(Clone, Default)]
struct OrderBook {
    bids: SideBook,
    asks: SideBook,
}

impl OrderBook {
    fn from_midprice(mid: f64) -> Self {
        let mut bids = SideBook::default();
        let mut asks = SideBook::default();

        for i in 0..20 {
            let level = i as f64;
            let spread = 0.5 * level.max(1.0);
            let bid_price = mid - spread;
            let ask_price = mid + spread;
            let size = 0.1 + level * 0.01;

            bids.levels.push((bid_price, size));
            asks.levels.push((ask_price, size));
        }

        Self { bids, asks }
    }

    /// Build cumulative depth points for a depth chart.
    fn depth_points(&self) -> (Vec<(f64, f64)>, Vec<(f64, f64)>) {
        // sort bids descending by price (high -> low)
        let mut bids = self.bids.levels.clone();
        bids.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        // sort asks ascending by price (low -> high)
        let mut asks = self.asks.levels.clone();
        asks.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

        let mut bid_points = Vec::new();
        let mut ask_points = Vec::new();

        let mut cum = 0.0;
        for (p, s) in bids {
            cum += s;
            bid_points.push((p, cum));
        }

        cum = 0.0;
        for (p, s) in asks {
            cum += s;
            ask_points.push((p, cum));
        }

        (bid_points, ask_points)
    }

    fn depth_bounds(&self) -> Option<(f64, f64, f64)> {
        let (bids, asks) = self.depth_points();
        if bids.is_empty() && asks.is_empty() {
            return None;
        }

        let mut min_price = f64::MAX;
        let mut max_price = f64::MIN;
        let mut max_size = 0.0;

        for (p, s) in bids.iter().chain(asks.iter()) {
            if *p < min_price {
                min_price = *p;
            }
            if *p > max_price {
                max_price = *p;
            }
            if *s > max_size {
                max_size = *s;
            }
        }

        if max_size <= 0.0 {
            max_size = 1.0;
        }

        let pad = (max_price - min_price) * 0.05;
        if pad > 0.0 {
            min_price -= pad;
            max_price += pad;
        }

        Some((min_price, max_price, max_size))
    }

    fn format_bids(&self, depth: usize) -> Vec<Line<'static>> {
        let mut bids = self.bids.levels.clone();
        // highest first
        bids.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        bids.into_iter()
            .take(depth)
            .map(|(p, s)| Line::from(format!("{:>10.2}  {:>8.4}", p, s)))
            .collect()
    }

    fn format_asks(&self, depth: usize) -> Vec<Line<'static>> {
        let mut asks = self.asks.levels.clone();
        // lowest first
        asks.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        asks.into_iter()
            .take(depth)
            .map(|(p, s)| Line::from(format!("{:>10.2}  {:>8.4}", p, s)))
            .collect()
    }
}

/// ----- CandleChart: ASCII candlesticks + grid lines -----

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

        let row_min = area.y as i32;
        let row_max = area.y as i32 + area.height as i32 - 1;

        // horizontal grid lines
        let grid_lines = 4;
        for i in 0..=grid_lines {
            let price = y_min + (span * i as f64 / grid_lines as f64);
            let row = map_price_to_row(price).clamp(row_min, row_max);
            for x in area.x..(area.x + area.width) {
                if let Some(cell) = buf.cell_mut((x, row as u16)) {
                    if cell.symbol() == " " {
                        cell.set_symbol("─").set_fg(Color::DarkGray);
                    }
                }
            }
        }

        // candles (wick + body)
        for (i, c) in self.candles[start..].iter().enumerate() {
            let x = area.x + i as u16;

            let low_row = map_price_to_row(c.low);
            let high_row = map_price_to_row(c.high);
            let open_row = map_price_to_row(c.open);
            let close_row = map_price_to_row(c.close);

            let color = if c.close >= c.open {
                Color::Green
            } else {
                Color::Red
            };

            // wick
            let wick_start = low_row.min(high_row).max(row_min);
            let wick_end = low_row.max(high_row).min(row_max);
            for y in wick_start..=wick_end {
                if let Some(cell) = buf.cell_mut((x, y as u16)) {
                    cell.set_symbol("│").set_fg(color);
                }
            }

            // body
            let body_start = open_row.min(close_row).max(row_min);
            let body_end = open_row.max(close_row).min(row_max);
            for y in body_start..=body_end {
                if let Some(cell) = buf.cell_mut((x, y as u16)) {
                    cell.set_symbol("█").set_fg(color);
                }
            }
        }
    }
}

/// Compute a simple RSI over closes
fn compute_rsi(closes: &[f64], period: usize) -> Vec<(f64, f64)> {
    if closes.len() < period + 1 {
        return Vec::new();
    }

    let mut out = Vec::new();

    for i in period..closes.len() {
        let window = &closes[i - period..=i];
        let mut gains = 0.0;
        let mut losses = 0.0;

        for w in 1..window.len() {
            let diff = window[w] - window[w - 1];
            if diff >= 0.0 {
                gains += diff;
            } else {
                losses -= diff; // diff negative
            }
        }

        let avg_gain = gains / period as f64;
        let avg_loss = losses / period as f64;

        let rsi = if avg_loss == 0.0 {
            100.0
        } else {
            let rs = avg_gain / avg_loss;
            100.0 - (100.0 / (1.0 + rs))
        };

        out.push((i as f64, rsi));
    }

    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Orderbook,
    Candles,
}

fn main() -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // ---- state ----
    let mut last_price = 3000.0;

    let mut tf_30s = CandleAgg::new(30);
    let mut tf_1m = CandleAgg::new(60);
    let mut tf_3m = CandleAgg::new(180);
    let mut tf_5m = CandleAgg::new(300);

    let mut selected_tf: u64 = 60; // default = 1m
    let mut selected_tab = Tab::Candles;

    loop {
        // input
        if event::poll(Duration::from_millis(1))? {
            if let Event::Key(k) = event::read()? {
                match k.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Char('o') => selected_tab = Tab::Orderbook,
                    KeyCode::Char('c') => selected_tab = Tab::Candles,
                    KeyCode::Char('1') => selected_tf = 30,
                    KeyCode::Char('2') => selected_tf = 60,
                    KeyCode::Char('3') => selected_tf = 180,
                    KeyCode::Char('4') => selected_tf = 300,
                    _ => {}
                }
            }
        }

        // real timestamp in seconds
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // random walk price
        let mut rng = rng();
        let step: f64 = rng.random_range(-2.0..2.0);
        last_price += step;
        last_price = last_price.clamp(2950.0, 3050.0);

        // synthetic orderbook built from current price
        let order_book = OrderBook::from_midprice(last_price);

        // feed candles
        tf_30s.update(ts, last_price);
        tf_1m.update(ts, last_price);
        tf_3m.update(ts, last_price);
        tf_5m.update(ts, last_price);

        // choose timeframe series
        let series = match selected_tf {
            30 => tf_30s.get_series(),
            60 => tf_1m.get_series(),
            180 => tf_3m.get_series(),
            300 => tf_5m.get_series(),
            _ => tf_1m.get_series(),
        };

        // draw
        terminal.draw(|f| {
            let area = f.area();

            // top tab/header
            let layout = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1),
                    Constraint::Min(1),
                ])
                .split(area);

            // tab bar
            let tabs_line = {
                let ob_style = if selected_tab == Tab::Orderbook {
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                let c_style = if selected_tab == Tab::Candles {
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };

                Paragraph::new(Line::from(vec![
                    Span::styled("[o] Orderbook+Depth  ", ob_style),
                    Span::styled("[c] Candles+RSI  ", c_style),
                    Span::raw(" | TF: 1=30s 2=1m 3=3m 4=5m  | q=quit"),
                ]))
            };
            f.render_widget(tabs_line, layout[0]);

            match selected_tab {
                Tab::Orderbook => draw_orderbook_view(f, layout[1], &order_book),
                Tab::Candles => draw_candles_view(f, layout[1], &series, selected_tf),
            }
        })?;

        // 30 FPS-ish; rendering rate does NOT affect candle boundaries
        std::thread::sleep(Duration::from_millis(33));
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}

/// Draw orderbook + depth view
fn draw_orderbook_view(
    f: &mut ratatui::Frame<'_>,
    area: Rect,
    ob: &OrderBook,
) {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(40),
            Constraint::Percentage(60),
        ])
        .split(area);

    // depth chart (top)
    if let Some((min_p, max_p, max_s)) = ob.depth_bounds() {
        let (bids_pts, asks_pts) = ob.depth_points();

        let bids_ds = Dataset::default()
            .name("Bids")
            .marker(Marker::Braille)
            .data(&bids_pts);

        let asks_ds = Dataset::default()
            .name("Asks")
            .marker(Marker::Braille)
            .data(&asks_pts);

        let x_axis = Axis::default()
            .title("Price")
            .bounds([min_p, max_p]);

        let y_axis = Axis::default()
            .title("Cum size")
            .bounds([0.0, max_s]);

        let chart = Chart::new(vec![bids_ds, asks_ds])
            .block(
                Block::default()
                    .title(" Depth ")
                    .borders(Borders::ALL),
            )
            .x_axis(x_axis)
            .y_axis(y_axis);

        f.render_widget(chart, v[0]);
    } else {
        let block = Block::default()
            .title(" Depth ")
            .borders(Borders::ALL);
        f.render_widget(block, v[0]);
    }

    // ladder (bottom)
    let h = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(50),
            Constraint::Percentage(50),
        ])
        .split(v[1]);

    let bids_lines = ob.format_bids(20);
    let asks_lines = ob.format_asks(20);

    let bids_widget = Paragraph::new(bids_lines).block(
        Block::default()
            .title(" BIDS ")
            .borders(Borders::ALL),
    );

    let asks_widget = Paragraph::new(asks_lines).block(
        Block::default()
            .title(" ASKS ")
            .borders(Borders::ALL),
    );

    f.render_widget(bids_widget, h[0]);
    f.render_widget(asks_widget, h[1]);
}

/// Draw candles + RSI view
fn draw_candles_view(
    f: &mut ratatui::Frame<'_>,
    area: Rect,
    series: &[Candle],
    selected_tf: u64,
) {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(60),
            Constraint::Percentage(20),
            Constraint::Percentage(20),
        ])
        .split(area);

    // --- Candles + grid + timestamps ---
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
                " Candles (real time) — TF: {}  (1=30s,2=1m,3=3m,4=5m) ",
                tf_label
            ),
            Style::default().add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL);

    let inner = block.inner(v[0]);
    f.render_widget(block, v[0]);

    if !series.is_empty() && inner.width > 0 && inner.height > 1 {
        // split inner into [chart; axis-row]
        let inner_v = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(inner.height.saturating_sub(1)),
                Constraint::Length(1),
            ])
            .split(inner);

        let chart_area = inner_v[0];
        let axis_area = inner_v[1];

        let window_len = chart_area.width as usize;
        let visible: Vec<Candle> = series
            .iter()
            .rev()
            .take(window_len)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();

        // y-bounds fixed for now
        let y_min = 2950.0;
        let y_max = 3050.0;

        let chart_widget = CandleChart::new(&visible, y_min, y_max);
        f.render_widget(chart_widget, chart_area);

        if let (Some(first), Some(last)) = (visible.first(), visible.last()) {
            let axis_text = Line::from(format!(
                "t_start: {}   t_end: {}   (unix sec)",
                first.t, last.t
            ));
            let axis_widget = Paragraph::new(axis_text);
            f.render_widget(axis_widget, axis_area);
        }
    }

    // --- RSI pane ---
    let closes: Vec<f64> = series.iter().map(|c| c.close).collect();
    let rsi_points = compute_rsi(&closes, 14);
    if !rsi_points.is_empty() {
        let min_x = rsi_points.first().map(|(x, _)| *x).unwrap_or(0.0);
        let max_x = rsi_points.last().map(|(x, _)| *x).unwrap_or(1.0);

        let ds = Dataset::default()
            .name("RSI(14)")
            .marker(Marker::Dot)
            .data(&rsi_points);

        let x_axis = Axis::default()
            .title("index")
            .bounds([min_x, max_x]);

        let y_axis = Axis::default()
            .title("RSI")
            .bounds([0.0, 100.0]);

        let rsi_chart = Chart::new(vec![ds])
            .block(
                Block::default()
                    .title(" RSI ")
                    .borders(Borders::ALL),
            )
            .x_axis(x_axis)
            .y_axis(y_axis);

        f.render_widget(rsi_chart, v[1]);
    } else {
        let block = Block::default()
            .title(" RSI ")
            .borders(Borders::ALL);
        f.render_widget(block, v[1]);
    }

    // --- Info pane ---
    let mut info_lines = Vec::new();
    if let Some(c) = series.last() {
        info_lines.push(Line::from(format!("Bucket start (unix sec): {}", c.t)));
        info_lines.push(Line::from(format!("Open  : {:.2}", c.open)));
        info_lines.push(Line::from(format!("High  : {:.2}", c.high)));
        info_lines.push(Line::from(format!("Low   : {:.2}", c.low)));
        info_lines.push(Line::from(format!("Close : {:.2}", c.close)));
    } else {
        info_lines.push(Line::from("No candles yet."));
    }

    info_lines.push(Line::from(""));
    info_lines.push(Line::from(
        "Candles & RSI driven by real wall-clock time (SystemTime).",
    ));

    let info = Paragraph::new(info_lines).block(
        Block::default()
            .title(" Info ")
            .borders(Borders::ALL),
    );

    f.render_widget(info, v[2]);
}
