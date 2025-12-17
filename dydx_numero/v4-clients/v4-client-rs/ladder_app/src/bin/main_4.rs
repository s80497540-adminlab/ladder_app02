use std::io;
use std::time::Duration;

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
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

#[derive(Clone, Copy)]
enum Side {
    Bid,
    Ask,
}

impl Side {
    fn toggle(self) -> Self {
        match self {
            Side::Bid => Side::Ask,
            Side::Ask => Side::Bid,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Side::Bid => "BID",
            Side::Ask => "ASK",
        }
    }
}

struct UiState {
    price: f64,
    size: f64,
    side: Side,
    log: Vec<String>,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            price: 3000.0,
            size: 0.1,
            side: Side::Bid,
            log: Vec::new(),
        }
    }
}

fn main() -> Result<()> {
    let mut state = UiState::default();

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    loop {
        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(k) = event::read()? {
                match k.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Char('w') | KeyCode::Up => state.price += 0.5,
                    KeyCode::Char('s') | KeyCode::Down => state.price -= 0.5,
                    KeyCode::Char('d') | KeyCode::Right => state.size += 0.01,
                    KeyCode::Char('a') | KeyCode::Left => {
                        state.size = (state.size - 0.01).max(0.0)
                    }
                    KeyCode::Tab => state.side = state.side.toggle(),
                    KeyCode::Enter => {
                        state.log.push(format!(
                            "Placed {} {:.4} @ {:.2}",
                            state.side.as_str(),
                            state.size,
                            state.price
                        ));
                        if state.log.len() > 10 {
                            state.log.remove(0);
                        }
                    }
                    KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => break,
                    _ => {}
                }
            }
        }

        terminal.draw(|f| {
            let area = f.area();

            // left: trading panel, right: log
            let chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(40),
                    Constraint::Percentage(60),
                ])
                .split(area);

            let side_str = state.side.as_str();
            let order_lines = vec![
                Line::from(format!("Side : {}", side_str)),
                Line::from(format!("Price: {:.2}", state.price)),
                Line::from(format!("Size : {:.4}", state.size)),
                Line::from(""),
                Line::from("Controls:"),
                Line::from("  W/S or ↑/↓ : price +/-"),
                Line::from("  A/D or ←/→ : size +/-"),
                Line::from("  Tab         : toggle side"),
                Line::from("  Enter       : submit"),
                Line::from("  q           : quit"),
            ];

            let order_widget = Paragraph::new(order_lines).block(
                Block::default()
                    .title(Span::styled(
                        " Trading Panel ",
                        Style::default().add_modifier(Modifier::BOLD),
                    ))
                    .borders(Borders::ALL),
            );

            f.render_widget(order_widget, chunks[0]);

            let mut log_lines: Vec<Line> = state
                .log
                .iter()
                .rev()
                .map(|l| Line::from(l.as_str()))
                .collect();
            if log_lines.is_empty() {
                log_lines.push(Line::from("No orders yet."));
            }

            let log_widget = Paragraph::new(log_lines).block(
                Block::default()
                    .title(" Order Log ")
                    .borders(Borders::ALL),
            );

            f.render_widget(log_widget, chunks[1]);
        })?;
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}
