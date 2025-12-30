pub mod commands;
pub mod event;
pub mod reducer;
pub mod render;
pub mod state;

pub use event::*;
pub use state::*;

use crate::AppWindow;

pub struct AppRuntime {
    pub state: AppState,
    ui: slint::Weak<AppWindow>,
    dirty: bool,
    last_tick_unix: u64,
}

impl AppRuntime {
    pub fn new(ui: slint::Weak<AppWindow>) -> Self {
        Self {
            state: AppState::default(),
            ui,
            dirty: true,
            last_tick_unix: 0,
        }
    }

    pub fn handle_event(&mut self, ev: AppEvent) {
        let changed = reducer::reduce(&mut self.state, ev);
        if changed {
            self.dirty = true;
        }
    }

    pub fn tick_if_needed(&mut self) {
        let now = crate::app::state::now_unix();
        if now != self.last_tick_unix {
            self.last_tick_unix = now;
            let changed = reducer::reduce(&mut self.state, AppEvent::Timer(TimerEvent::Tick1s { now_unix: now }));
            if changed {
                self.dirty = true;
            }
        }
    }

    pub fn render(&mut self) {
        if let Some(ui) = self.ui.upgrade() {
            render::render(&self.state, &ui);
            self.dirty = false;
        }
    }

    pub fn render_if_dirty(&mut self) {
        if self.dirty {
            self.render();
        }
    }
}
