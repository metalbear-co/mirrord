use std::time::{Duration, Instant};

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Flex, Layout, Rect},
    style::Style,
    text::Span,
    widgets::{Clear, StatefulWidget, Widget},
};
use throbber_widgets_tui::{Throbber, ThrobberState, WhichUse};

use crate::theme;

const EMPTY: &str = "•";

/// Indicator of ongoing work and failure.
///
/// Draws a spinner when `working` is `true` and an error icon when `failed` is `true`.
#[must_use]
pub struct Activity {
    working: bool,
    failed: bool,
}

impl Activity {
    /// Creates an indicator.
    pub fn new(working: bool, failed: bool) -> Self {
        Self { working, failed }
    }
}

/// State for the [`Activity`] widget.
#[derive(Default)]
#[must_use]
pub struct ActivityState {
    throbber_state: ThrobberState,
    rendered_at: Option<Instant>,
    elapsed: Duration,
}

impl StatefulWidget for Activity {
    type State = ActivityState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        const FREQUENCY: Duration = Duration::from_millis(250);

        state.elapsed += state.rendered_at.map(|at| at.elapsed()).unwrap_or_default();

        let advanced = state.elapsed.as_millis() / FREQUENCY.as_millis();

        for _ in 0..advanced {
            state.throbber_state.calc_next();
        }

        state.elapsed -= FREQUENCY * advanced as u32;
        state.rendered_at = Some(Instant::now());

        Clear.render(area, buf);

        let [a, _, b] = Layout::horizontal([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .flex(Flex::Center)
        .areas(area);

        if self.working {
            let throbber = Throbber::default()
                .throbber_style(theme::muted())
                .throbber_set(throbber_widgets_tui::BRAILLE_EIGHT)
                .use_type(WhichUse::Spin);

            StatefulWidget::render(throbber, a, buf, &mut state.throbber_state);
        } else {
            Span::raw(EMPTY).render(a, buf);
        }

        if self.failed {
            Span::styled("!", Style::default().on_red().white().bold()).render(b, buf);
        } else {
            Span::raw(EMPTY).render(b, buf);
        }
    }
}
