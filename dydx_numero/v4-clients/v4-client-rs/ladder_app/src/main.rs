use std::io;
use std::time::Duration as StdDuration;

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Chart, Axis, Dataset},
    Terminal,
};
use ratatui::symbols::Marker;
use tokio::sync::watch;
use tracing_subscriber;

// prove the crate links; we won't use it yet
use dydx_client as _;

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
    fn fake() -> Self {
        let mut bids = SideBook::default();
        let mut asks = SideBook::default();

        let mid = 3000.0;

        for i in 0..20 {
            let level = i as f64;
            bids.levels.push((mid - level * 0.5, 0.1 + level * 0.01));
            asks.levels.push((mid + level * 0.5, 0.1 + level * 0.01));
        }

        Self { bids, asks }
    }

    fn format_bids(&self, depth: usize) -> Vec<Line<'static>> {
        self.bids
            .levels
            .iter()
            .take(depth)
            .map(|(price, size)| {
                Line::from(vec![
                    Span::raw(format!("{:>12.2}", price)),
                    Span::raw("  "),
                    Span::raw(format!("{:>12.4}", size)),
                ])
            })
            .collect()
    }

    fn format_asks(&self, depth: usize) -> Vec<Line<'static>> {
        self.asks
            .levels
            .iter()
            .take(depth)
            .map(|(price, size)| {
                Line::from(vec![
                    Span::raw(format!("{:>12.2}", price)),
                    Span::raw("  "),
                    Span::raw(format!("{:>12.4}", size)),
                ])
            })
            .collect()
    }

    /// Build cumulative depth points for a depth chart.
    /// Returns (bids_points, asks_points) as Vec<(x=price, y=cum_size)>.
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

    /// Bounds for the chart axes (min_price, max_price, max_size)
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

        // add a bit of padding
        let pad = (max_price - min_price) * 0.05;
        if pad > 0.0 {
            min_price -= pad;
            max_price += pad;
        }

        Some((min_price, max_price, max_size))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let (tx, rx) = watch::channel(OrderBook::fake());

    // background task to slowly nudge prices so you see movement
    tokio::spawn(async move {
        let mut book = OrderBook::fake();
        loop {
            for (p, _) in &mut book.bids.levels {
                *p += 0.1;
            }
            for (p, _) in &mut book.asks.levels {
                *p += 0.1;
            }
            let _ = tx.send(book.clone());
            tokio::time::sleep(StdDuration::from_millis(500)).await;
        }
    });

    run_ui(rx)?;

    Ok(())
}

fn run_ui(mut rx: watch::Receiver<OrderBook>) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let res = ui_loop(&mut terminal, &mut rx);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    res
}

fn ui_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    rx: &mut watch::Receiver<OrderBook>,
) -> Result<()> {
    loop {
        if event::poll(StdDuration::from_millis(10))? {
            if let Event::Key(key) = event::read()? {
                if key.code == KeyCode::Char('q') {
                    break;
                }
            }
        }

        let book = rx.borrow().clone();

        terminal.draw(|f| {
            let area = f.area();

            // top: chart, bottom: ladder
            let vertical = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Percentage(40),
                    Constraint::Percentage(60),
                ])
                .split(area);

            let chart_area = vertical[0];
            let ladder_area = vertical[1];

            // bottom split into bids/asks
            let ladder_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(50),
                    Constraint::Percentage(50),
                ])
                .split(ladder_area);

            // ---- depth chart ----
            if let Some((min_price, max_price, max_size)) = book.depth_bounds() {
                let (bid_points, ask_points) = book.depth_points();

                let bids_dataset = Dataset::default()
                    .name("Bids")
                    .marker(Marker::Braille)
                    .data(&bid_points);

                let asks_dataset = Dataset::default()
                    .name("Asks")
                    .marker(Marker::Braille)
                    .data(&ask_points);

                let x_axis = Axis::default()
                    .title("Price")
                    .bounds([min_price, max_price]);

                let y_axis = Axis::default()
                    .title("Cum Size")
                    .bounds([0.0, max_size]);

                let chart = Chart::new(vec![bids_dataset, asks_dataset])
                    .block(
                        Block::default()
                            .title(Span::styled(
                                " Depth ",
                                Style::default().add_modifier(Modifier::BOLD),
                            ))
                            .borders(Borders::ALL),
                    )
                    .x_axis(x_axis)
                    .y_axis(y_axis);

                f.render_widget(chart, chart_area);
            } else {
                // if no data, just show an empty block
                let block = Block::default()
                    .title(" Depth ")
                    .borders(Borders::ALL);
                f.render_widget(block, chart_area);
            }

            // ---- ladder ----
            let bids_lines = book.format_bids(20);
            let asks_lines = book.format_asks(20);

            let bids_widget = Paragraph::new(bids_lines).block(
                Block::default()
                    .title(Span::styled(
                        " BIDS ",
                        Style::default().add_modifier(Modifier::BOLD),
                    ))
                    .borders(Borders::ALL),
            );

            let asks_widget = Paragraph::new(asks_lines).block(
                Block::default()
                    .title(Span::styled(
                        " ASKS ",
                        Style::default().add_modifier(Modifier::BOLD),
                    ))
                    .borders(Borders::ALL),
            );

            f.render_widget(bids_widget, ladder_chunks[0]);
            f.render_widget(asks_widget, ladder_chunks[1]);
        })?;
    }

    Ok(())
}
