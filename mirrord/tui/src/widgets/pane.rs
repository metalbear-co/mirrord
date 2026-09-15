use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Flex, Layout, Rect},
    symbols,
    widgets::{Block, StatefulWidget, Widget},
};

use crate::widgets::activity::{Activity, ActivityState};

/// A generic pane.
#[must_use]
pub struct Pane {
    working: bool,
    failed: bool,
}

impl Pane {
    /// Creates a new pane.
    pub fn new(working: bool, failed: bool) -> Self {
        Self { working, failed }
    }
}

/// State for the [`Pane`] widget.
#[derive(Default)]
#[must_use]
pub struct PaneState {
    activity_state: ActivityState,
}

impl Pane {
    // Computes the inner area of the pane the content should be rendered in.
    pub fn inner(&self, area: Rect) -> Rect {
        self.block().inner(area)
    }

    fn block(&self) -> Block<'_> {
        Block::bordered().border_set(symbols::border::ROUNDED)
    }
}

impl StatefulWidget for Pane {
    type State = PaneState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        self.block().render(area, buf);

        let [right] = Layout::vertical([Constraint::Length(1)])
            .flex(Flex::Start)
            .areas(area);

        let [corner] = Layout::horizontal([Constraint::Length(5)])
            .horizontal_margin(1)
            .flex(Flex::End)
            .areas(right);

        Activity::new(self.working, self.failed).render(corner, buf, &mut state.activity_state);
    }
}
