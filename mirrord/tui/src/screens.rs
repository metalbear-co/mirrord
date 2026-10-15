use std::ops::ControlFlow;

use crossterm::event::Event;
use ratatui::{Frame, layout::Rect};

use crate::{context::Context, kit::hints::Hint};

pub mod config;
pub mod databases;
pub mod home;
pub mod preview_envs;
pub mod queues;
pub mod resources;
pub mod run;
pub mod sessions;
pub mod targets;
pub mod terminal;

/// A single full-screen view.
pub trait Screen {
    /// Creates a new instance of this screen.
    fn new(context: Context) -> Self
    where
        Self: Sized;

    /// Renders itself into `area`.
    fn draw(&mut self, frame: &mut Frame, area: Rect);

    /// Handles an event.
    fn handle_event(&mut self, event: Event) -> ControlFlow<(), Event> {
        ControlFlow::Continue(event)
    }

    /// The keys this screen answers to in its current state, for the `?` help dialog.
    fn hints(&self) -> Vec<Hint> {
        Vec::new()
    }

    /// A view the screen asks the application to switch to, by the name `:` takes for it, such as
    /// `sessions`. Taken once: the application calls this after every event and switches when it
    /// gets one.
    fn take_view_request(&mut self) -> Option<&'static str> {
        None
    }

    /// Edits this screen holds that are not saved anywhere yet, described for the dialog that
    /// asks before quitting would lose them.
    fn unsaved_changes(&self) -> Option<String> {
        None
    }

    /// Saves what [`Screen::unsaved_changes`] describes. Returns why it could not, in which case
    /// the application does not quit.
    fn save_changes(&mut self) -> Result<(), String> {
        Ok(())
    }
}
