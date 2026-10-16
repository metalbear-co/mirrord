//! Key hints: what each key does in the current view.
//!
//! Views describe their keys as [`Hint`]s instead of writing hint strings by hand, so the same
//! list feeds both the key bar at the bottom of the screen and the `?` help dialog.

use std::borrow::Cow;

use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

use crate::theme;

/// One key, or a set of keys, and what pressing it does.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Hint {
    pub keys: Cow<'static, str>,
    pub action: Cow<'static, str>,
}

impl Hint {
    pub const fn new(keys: &'static str, action: &'static str) -> Self {
        Self {
            keys: Cow::Borrowed(keys),
            action: Cow::Borrowed(action),
        }
    }

    /// A hint whose text is only known at runtime, such as `Enter target deployment/api`.
    pub fn owned(keys: String, action: String) -> Self {
        Self {
            keys: Cow::Owned(keys),
            action: Cow::Owned(action),
        }
    }
}

/// The hints as the bottom key bar draws them: `<key> action`, the keys in the brand color so
/// they stand out from what they do.
pub fn key_bar(hints: &[Hint]) -> Line<'static> {
    let mut spans = Vec::with_capacity(hints.len() * 3);
    for hint in hints {
        spans.push(Span::styled(
            format!("<{}>", hint.keys),
            Style::default()
                .fg(theme::INDIGO)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(format!(" {}  ", hint.action), theme::muted()));
    }
    Line::from(spans)
}

/// The hints as a legend inside a view, keys drawn as small keycaps (` space ` select), for the
/// few keys a view cannot be used without. Unlike the key bar, it sits next to what it explains.
pub fn legend(hints: &[Hint]) -> Line<'static> {
    let mut spans = Vec::with_capacity(hints.len() * 3);
    for hint in hints {
        spans.push(Span::styled(
            format!(" {} ", hint.keys),
            Style::default()
                .fg(theme::LAVENDER)
                .bg(theme::BORDER_DIM)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(format!(" {}   ", hint.action), theme::muted()));
    }
    Line::from(spans)
}
