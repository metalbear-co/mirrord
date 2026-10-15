//! A yes/no dialog for actions that cannot be undone, such as killing a session.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    style::Modifier,
    text::{Line, Span},
    widgets::{Block, BorderType, Clear, Paragraph},
};

use crate::{
    helpers::{centered, wrap},
    theme,
};

/// Widest the dialog gets, so its question reads as a sentence rather than a banner.
const WIDTH: u16 = 64;

pub struct Confirm {
    /// The question, such as `Kill session a3f9c1?`.
    question: String,
    /// What the action affects, one item per line.
    details: Vec<String>,
}

/// What a key press in the dialog decided.
pub enum Answer {
    Pending,
    Yes,
    No,
}

impl Confirm {
    pub fn new(question: String, details: Vec<String>) -> Self {
        Self { question, details }
    }

    /// `Enter` or `y` confirms; `Esc` or `n` cancels; every other key is ignored, so a stray
    /// press can never confirm.
    pub fn handle_key(&self, key: KeyEvent) -> Answer {
        match key.code {
            KeyCode::Enter | KeyCode::Char('y') => Answer::Yes,
            KeyCode::Esc | KeyCode::Char('n') => Answer::No,
            _ => Answer::Pending,
        }
    }

    pub fn draw(&self, frame: &mut Frame, area: Rect) {
        let width = WIDTH.min(area.width.saturating_sub(4));
        let inner_width = usize::from(width.saturating_sub(4)).max(1);

        let mut lines: Vec<Line> = wrap(&self.question, inner_width)
            .into_iter()
            .map(|line| Line::styled(line, theme::error().add_modifier(Modifier::BOLD)))
            .collect();
        lines.push(Line::raw(""));
        for detail in &self.details {
            lines.extend(
                wrap(detail, inner_width)
                    .into_iter()
                    .map(|line| Line::styled(line, theme::muted())),
            );
        }
        lines.push(Line::raw(""));
        lines.push(Line::from_iter([
            Span::styled("<enter>", theme::error().add_modifier(Modifier::BOLD)),
            Span::styled(" confirm  ", theme::muted()),
            Span::styled("<esc>", theme::title()),
            Span::styled(" cancel", theme::muted()),
        ]));

        let height = (lines.len() as u16 + 2).min(area.height);
        let dialog = centered(area, Constraint::Length(width), Constraint::Length(height));
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(theme::error())
            .padding(ratatui::widgets::Padding::horizontal(1));
        let inner = block.inner(dialog);
        frame.render_widget(Clear, dialog);
        frame.render_widget(block, dialog);
        frame.render_widget(Paragraph::new(lines), inner);
    }
}
