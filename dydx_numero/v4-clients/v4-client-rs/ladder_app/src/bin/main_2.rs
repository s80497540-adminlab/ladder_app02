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
    widgets::{Block, Borders, Paragraph},
    Terminal,
};
use tokio::sync::watch;
use tracing_subscriber;

// we import the crate just to confirm linking works, but we won't use it yet
use dydx_client as _;

/// One side of the book (bids or asks)
#[derive(Clone, Default)]
struct SideBook {
    levels: Vec<(f64, f64)>, // (price, size)
}

/// Simple in-memory orderbook model for the UI.
#[derive(Clone, Default)]
struct OrderBook {
    bids: SideBook, // highest price first
    asks: SideBook, // lowest price first
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
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    // watch channel just to keep the structure the same as before
    let (tx, rx) = watch::channel(OrderBook::fake());

    // background task that periodically updates the fake book (optional)
    tokio::spawn(async move {
        let mut book = OrderBook::fake();
        loop {
            // simple fake mid-price move
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
        // 'q' to quit
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

            let chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(50),
                    Constraint::Percentage(50),
                ])
                .split(area);

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

            f.render_widget(bids_widget, chunks[0]);
            f.render_widget(asks_widget, chunks[1]);
        })?;
    }

    Ok(())
}
