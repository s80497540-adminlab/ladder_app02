pub mod commands;
pub mod event;
pub mod reducer;
pub mod render;
pub mod state;

pub use event::*;
pub use state::*;

use crate::AppWindow;
use std::time::Instant;

pub struct AppRuntime {
    pub state: AppState,
    ui: slint::Weak<AppWindow>,
    dirty: bool,
    last_tick_unix: u64,
    last_render_instant: Option<Instant>,
}

impl AppRuntime {
    pub fn new(ui: slint::Weak<AppWindow>) -> Self {
        Self {
            state: AppState::default(),
            ui,
            dirty: true,
            last_tick_unix: 0,
            last_render_instant: None,
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
            const MIN_FRAME_MS: u64 = 16; // ~60 FPS target for smoother pan/zoom
            if let Some(last) = self.last_render_instant {
                let since = last.elapsed().as_millis() as u64;
                if since < MIN_FRAME_MS {
                    return;
                }
            }
            self.render();
            self.last_render_instant = Some(Instant::now());
        }
    }

    pub fn process_pending_history(&mut self) {
        if !self.state.chart_enabled || !self.state.history_valve_open {
            return;
        }
        const HISTORY_BUDGET_MS: u64 = 3;
        let start = Instant::now();
        let mut changed = false;
        while start.elapsed().as_millis() < HISTORY_BUDGET_MS as u128 {
            if self.state.pending_mid_ticks.is_empty() && !self.state.history_loading {
                break;
            }
            if self.state.process_pending_history(500) {
                changed = true;
            } else {
                break;
            }
        }
        if changed {
            self.dirty = true;
        }
    }

    pub fn update_perf(&mut self, frame_ms: f32, events: usize) {
        if self.state.update_perf(frame_ms, events) {
            self.dirty = true;
        }
    }
}
