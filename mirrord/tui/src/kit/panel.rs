//! The frame every list and view is drawn in, titled the way k9s titles its views:
//! `sessions[2]` centered on the top border, a note such as when the data was refreshed on the
//! right, and a short-lived message on the bottom border.

use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Padding},
};

use crate::theme;

/// Lights up the border of the panel that has the keyboard, when a view shows several.
pub fn focus(block: Block<'static>, focused: bool) -> Block<'static> {
    if focused {
        block.border_style(Style::default().fg(theme::INDIGO))
    } else {
        block
    }
}

/// A bordered panel titled `name[count]`.
///
/// `count` is drawn in amber so it reads at a glance; `note` sits muted on the right of the top
/// border, `message` on the bottom border in its own style.
pub fn block(
    name: &str,
    count: Option<&str>,
    note: Option<&str>,
    message: Option<(&str, Style)>,
) -> Block<'static> {
    let mut title = vec![Span::styled(
        format!(" {name}"),
        Style::default()
            .fg(theme::INDIGO)
            .add_modifier(Modifier::BOLD),
    )];
    if let Some(count) = count {
        title.push(Span::styled("[", theme::muted()));
        title.push(Span::styled(
            count.to_owned(),
            Style::default()
                .fg(theme::AMBER)
                .add_modifier(Modifier::BOLD),
        ));
        title.push(Span::styled("]", theme::muted()));
    }
    title.push(Span::raw(" "));

    let mut block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::FILL_DIM))
        .padding(Padding::horizontal(1))
        .title(Line::from(title));
    if let Some(note) = note {
        block = block.title_top(Line::styled(format!(" {note} "), theme::muted()).right_aligned());
    }
    if let Some((message, style)) = message {
        block = block.title_bottom(Line::styled(format!(" {message} "), style).left_aligned());
    }
    block
}
