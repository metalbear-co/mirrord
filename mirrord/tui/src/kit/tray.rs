//! A row of chips naming what has been picked, each with a `✕` that unpicks it on a click.
//!
//! It answers "what have I picked, and how do I undo it" without leaving the list: everything
//! picked is on view (`+N more` when it does not fit), and removing one is a click or, from the
//! keyboard, `Space` on its row in the list.

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::{helpers::ellipsize, theme};

/// Longest a chip's text gets before it is cut.
const MAX_CHIP: usize = 36;

#[derive(Default)]
pub struct Tray {
    /// Where each chip landed in the last frame, and the id it stands for.
    hits: Vec<(Rect, String)>,
}

impl Tray {
    /// Draws `label` and one chip per `(id, text)`, or `empty` in muted text when there are none.
    pub fn draw(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        label: &str,
        chips: &[(String, String)],
        empty: &str,
    ) {
        self.hits.clear();
        let mut spans = vec![Span::styled(
            format!("{label} "),
            Style::default()
                .fg(theme::INDIGO_LIGHT)
                .add_modifier(Modifier::BOLD),
        )];
        if chips.is_empty() {
            spans.push(Span::styled(empty.to_owned(), theme::muted().italic()));
            frame.render_widget(Paragraph::new(Line::from(spans)), area);
            return;
        }

        let mut x = area.x + u16::try_from(label.chars().count() + 1).unwrap_or(u16::MAX);
        let right = area.x + area.width;
        for (index, (id, text)) in chips.iter().enumerate() {
            let text = format!(" {} ", ellipsize(text, MAX_CHIP));
            let width = u16::try_from(text.chars().count() + 3).unwrap_or(u16::MAX);
            let rest = chips.len() - index;
            // Leave room for `+N more` unless this is the last chip.
            let reserve = if rest > 1 { 9 } else { 0 };
            if x + width + reserve > right {
                spans.push(Span::styled(format!("+{rest} more"), theme::muted()));
                break;
            }
            spans.push(Span::styled(
                text,
                Style::default()
                    .fg(theme::LAVENDER)
                    .bg(theme::DEEP)
                    .add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(
                "\u{2715}",
                Style::default().fg(theme::CORAL).bg(theme::DEEP),
            ));
            spans.push(Span::raw("  "));
            self.hits
                .push((Rect::new(x, area.y, width - 2, 1), id.clone()));
            x += width;
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    /// The id of the chip a left click landed on, to unpick it.
    pub fn clicked(&self, mouse: &MouseEvent) -> Option<String> {
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return None;
        }
        let at = Position::new(mouse.column, mouse.row);
        self.hits
            .iter()
            .find(|(area, _)| area.contains(at))
            .map(|(_, id)| id.clone())
    }
}
