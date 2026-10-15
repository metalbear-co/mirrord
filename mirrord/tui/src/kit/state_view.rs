//! What a list shows when it has no rows to show.
//!
//! Every list that is read from the cluster goes through the same states - waiting for a
//! connection, loading, failed, loaded but empty, emptied by a search - and each one gets the same
//! wording and color on every screen, so the user can always tell "nothing there" from "still
//! asking" from "something broke".

use std::{sync::LazyLock, time::Instant};

use k8s_openapi::jiff::{Timestamp, tz::TimeZone};
use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    style::Style,
    text::Span,
    widgets::Paragraph,
};

use crate::{helpers::centered, theme};

/// How long each spinner frame stays up.
const SPINNER_FRAME_MS: u128 = 125;

/// When the spinner started turning. Frames are picked from the time since, so every spinner on
/// screen turns in step without each list keeping its own spinner state.
static SPINNER_START: LazyLock<Instant> = LazyLock::new(Instant::now);

/// The outcome of the latest attempt to read something from the cluster.
#[derive(Debug, Default)]
pub enum Fetch<T> {
    /// The application is not connected to the cluster.
    Disconnected,
    /// The first attempt is still in flight.
    #[default]
    Loading,
    /// The attempt failed.
    Failed(String),
    /// The attempt succeeded.
    Ready(T),
}

/// A list read from the cluster in the background, as the screen drawing it sees it.
pub struct Listing<T> {
    pub state: Fetch<Vec<T>>,
    /// When [`Listing::state`] was last replaced.
    updated_at: Timestamp,
}

// Written out because the derive would require `T: Default`, which a list of items never needs.
impl<T> Default for Listing<T> {
    fn default() -> Self {
        Self {
            state: Fetch::default(),
            updated_at: Timestamp::default(),
        }
    }
}

impl<T> Listing<T> {
    /// Replaces the state with the outcome of a new attempt.
    pub fn replace(&mut self, state: Fetch<Vec<T>>) {
        self.state = state;
        self.updated_at = Timestamp::now();
    }

    /// How many items there are, for a panel title: `3`, or `2/3` while a search leaves only
    /// `shown` of them. `None` until the list loads.
    pub fn count(&self, shown: usize) -> Option<String> {
        match &self.state {
            Fetch::Ready(items) if shown < items.len() => Some(format!("{shown}/{}", items.len())),
            Fetch::Ready(items) => Some(items.len().to_string()),
            _ => None,
        }
    }

    /// When the state was last replaced, in local time, for the panel title.
    pub fn updated(&self) -> String {
        let updated_at = self.updated_at.to_zoned(TimeZone::system());
        format!("updated {}", updated_at.strftime("%H:%M:%S"))
    }
}

/// The wording a list uses for its two kinds of empty.
pub struct EmptyText {
    /// The cluster has nothing to list, such as `No active sessions`.
    pub nothing: &'static str,
    /// The search filtered out every row, such as `No sessions match the search`.
    pub no_match: &'static str,
}

/// Draws the message that stands in for the rows, and returns `true`, when there are no rows on
/// screen. Returns `false`, drawing nothing, when `shown` rows are there to draw.
pub fn draw<T>(
    frame: &mut Frame,
    area: Rect,
    fetch: &Fetch<T>,
    shown: usize,
    searching: bool,
    text: &EmptyText,
) -> bool {
    let (message, style) = match fetch {
        Fetch::Ready(_) if shown > 0 => return false,
        Fetch::Disconnected => ("Waiting for a connection to the cluster...", theme::muted()),
        Fetch::Loading => {
            draw_message(
                frame,
                area,
                &format!("{} Loading...", spinner()),
                theme::muted(),
            );
            return true;
        }
        Fetch::Failed(error) => (error.as_str(), theme::error()),
        Fetch::Ready(_) if searching => (text.no_match, theme::muted()),
        Fetch::Ready(_) => (text.nothing, theme::muted()),
    };

    draw_message(frame, area, message, style);
    true
}

/// Draws one line of text centered in `area`.
pub fn draw_message(frame: &mut Frame, area: Rect, message: &str, style: Style) {
    frame.render_widget(
        Paragraph::new(Span::styled(message.to_owned(), style)).centered(),
        centered(area, Constraint::Fill(1), Constraint::Length(1)),
    );
}

/// The current spinner frame. The application redraws a few times a second, which is what turns
/// it.
fn spinner() -> &'static str {
    let symbols = throbber_widgets_tui::BRAILLE_EIGHT.symbols;
    let frame = SPINNER_START.elapsed().as_millis() / SPINNER_FRAME_MS;
    let index = usize::try_from(frame % symbols.len() as u128).unwrap_or_default();
    symbols.get(index).copied().unwrap_or_default()
}

/// What to tell the user when the cluster refused to list `what`, in place of the client's own
/// error text. The answers that have a clear next step get a sentence saying it; anything else
/// keeps the error as it is.
pub fn explain(error: &kube::Error, what: &str) -> String {
    let kube::Error::Api(status) = error else {
        return error.to_string();
    };
    match status.code {
        // The operator's API is registered but nothing answers behind it.
        503 => "The mirrord operator is not answering. Is it running?".to_owned(),
        404 => format!("This cluster does not serve {what}. Is the mirrord operator installed?"),
        403 => format!("Not allowed to list {what}: {}", status.message),
        401 => "The cluster did not accept your credentials. Log in again and press r.".to_owned(),
        _ => error.to_string(),
    }
}
