use std::ops::ControlFlow;

use crossterm::event::Event;
use ratatui::{Frame, layout::Rect};

use crate::context::Context;

pub mod databases;
pub mod home;
pub mod preview_envs;
pub mod queues;
pub mod sessions;
pub mod targets;
pub mod terminal;

/// A single full-screen view.
pub trait Screen {
    /// Creates a new instance of this screen.
    fn new(context: Context) -> Self;

    /// Renders itself into `area`.
    fn draw(&mut self, frame: &mut Frame, area: Rect);

    /// Handles an event.
    fn handle_event(&mut self, event: Event) -> ControlFlow<(), Event> {
        ControlFlow::Continue(event)
    }
}
