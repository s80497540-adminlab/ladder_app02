use std::io;
use std::thread;
use std::time::Duration;

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use rand::{thread_rng, Rng};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::Span,
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
    Terminal,
};

#[derive(Clone, Copy)]
enum TapeSide {
    Buy,
    Sell,
}

impl TapeSide {
    fn as_str(self) -> &'static str {
        match self {
            TapeSide::Buy => "B",
            TapeSide::Sell => "S",
        }
    }

    fn color(self) -> Color {
        match self {
            TapeSide::Buy => Color::Green,
            TapeSide::Sell => Color::Red,
        }
    }
}

#[derive(Clone)]
struct Trade {
    id: u64,
    price: f64,
    size: f64,
    side: TapeSide,
}

struct TapeState {
    trades: Vec<Trade>,
    selected: usize,
    next_id: u64,
}

impl TapeState {
    fn new() -> Self {
        Self {
            trades: Vec::new(),
            selected: 0,
            next_id: 1,
        }
    }

    fn push_trade(&mut self, t: Trade) {
        self.trades.push(t);
        if self.trades.len() > 100 {
            self.trades.remove(0);
        }
        if self.selected >= self.trades.len() {
            self.selected = self.trades.len().saturating_sub(1);
        }
    }

    fn len(&self) -> usize {
        self.trades.len()
    }

    fn selected_trade(&self) -> Option<&Trade> {
        self.trades.get(self.selected)
    }

    fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    fn move_down(&mut self) {
        if self.selected + 1 < self.trades.len() {
            self.selected += 1;
        }
    }

    fn gen_fake_trade(&mut self) {
        let mut rng = thread_rng();
        let base_price = 3000.0;
        let price = base_price + rng.gen_range(-50.0..50.0);
        let size = rng.gen_range(0.01..1.0);
        let side = if rng.gen_bool(0.5) {
            TapeSide::Buy
        } else {
            TapeSide::Sell
        };

        let t = Trade {
            id: self.next_id,
            price,
            size,
            side,
        };
        self.next_id += 1;
        self.push_trade(t);
    }
}

fn main() -> Result<()> {
    let mut state = TapeState::new();

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    loop {
        // generate a fake trade every 100ms-ish
        state.gen_fake_trade();

        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(k) = event::read()? {
                match k.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Char('w') | KeyCode::Up => state.move_up(),
                    KeyCode::Char('s') | KeyCode::Down => state.move_down(),
                    KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => break,
                    _ => {}
                }
            }
        }

        terminal.draw(|f| {
            let area = f.area();

            let chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(70),
                    Constraint::Percentage(30),
                ])
                .split(area);

            // left: tape
            let items: Vec<ListItem> = state
                .trades
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    let side_span = Span::styled(
                        t.side.as_str(),
                        Style::default()
                            .fg(t.side.color())
                            .add_modifier(if i == state.selected {
                                Modifier::BOLD
                            } else {
                                Modifier::empty()
                            }),
                    );
                    let text = format!(
                        " {:>4} | {:>6.2} | {:>7.4}",
                        t.id, t.price, t.size
                    );
                    ListItem::new(Span::raw(format!("{} {}", side_span.content, text)))
                })
                .collect();

            let mut list_state = ListState::default();
            if state.len() > 0 {
                list_state.select(Some(state.selected));
            }

            let list = List::new(items)
                .block(
                    Block::default()
                        .title(" Tape (W/S or ↑/↓ to move, q to quit) ")
                        .borders(Borders::ALL),
                )
                .highlight_style(
                    Style::default()
                        .add_modifier(Modifier::REVERSED),
                );

            f.render_stateful_widget(list, chunks[0], &mut list_state);

            // right: selected trade details
            let info_lines = if let Some(t) = state.selected_trade() {
                vec![
                    format!("ID   : {}", t.id),
                    format!("Side : {}", t.side.as_str()),
                    format!("Price: {:.2}", t.price),
                    format!("Size : {:.4}", t.size),
                ]
            } else {
                vec!["No trades yet.".to_string()]
            };

            let info_widget = Paragraph::new(
                info_lines
                    .into_iter()
                    .map(|s| s.into())
                    .collect::<Vec<ratatui::text::Line>>(),
            )
            .block(
                Block::default()
                    .title(" Selected ")
                    .borders(Borders::ALL),
            );

            f.render_widget(info_widget, chunks[1]);
        })?;

        thread::sleep(Duration::from_millis(80));
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}
