//! A centered pick-one dialog: a filterable list with the item currently
//! in effect marked by a dot.
//!
//! Typing narrows the list fzf-style (arrows move, Enter picks, Esc first
//! clears the filter and then closes), so long context or namespace lists
//! stay usable.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::{
    helpers::{centered, dialog},
    theme,
};

/// Rows shown at once; longer lists scroll under the selection.
const VISIBLE_ROWS: usize = 12;

/// What a key press in the picker resulted in.
pub enum PickerOutcome {
    /// Still open.
    Open,
    Cancelled,
    Picked(String),
}

pub struct Picker {
    title: &'static str,
    items: Vec<String>,
    /// The item currently in effect, marked in the list.
    active: Option<String>,
    /// Items are still being fetched.
    loading: bool,
    filter: String,
    selected: usize,
}

impl Picker {
    pub fn new(title: &'static str, items: Vec<String>, active: Option<String>) -> Self {
        let selected = active
            .as_deref()
            .and_then(|active| items.iter().position(|item| item == active))
            .unwrap_or(0);
        Self {
            title,
            items,
            active,
            loading: false,
            filter: String::new(),
            selected,
        }
    }

    /// A picker whose items are still on their way.
    pub fn loading(title: &'static str, active: Option<String>) -> Self {
        Self {
            loading: true,
            ..Self::new(title, Vec::new(), active)
        }
    }

    pub fn is_loading(&self) -> bool {
        self.loading
    }

    /// Fills the items once an async fetch lands.
    pub fn set_items(&mut self, items: Vec<String>) {
        self.loading = false;
        self.selected = self
            .active
            .as_deref()
            .and_then(|active| items.iter().position(|item| item == active))
            .unwrap_or(0);
        self.items = items;
    }

    fn visible(&self) -> Vec<&String> {
        let needle = self.filter.to_lowercase();
        self.items
            .iter()
            .filter(|item| item.to_lowercase().contains(&needle))
            .collect()
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> PickerOutcome {
        let count = self.visible().len();
        match key.code {
            // Esc peels one layer: filter first, then the dialog.
            KeyCode::Esc if !self.filter.is_empty() => {
                self.filter.clear();
                self.selected = 0;
            }
            KeyCode::Esc => return PickerOutcome::Cancelled,
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => self.selected = (self.selected + 1).min(count.saturating_sub(1)),
            KeyCode::Enter => {
                if let Some(item) = self.visible().get(self.selected) {
                    return PickerOutcome::Picked((*item).clone());
                }
            }
            KeyCode::Backspace => {
                self.filter.pop();
                self.selected = 0;
            }
            KeyCode::Char(c) => {
                self.filter.push(c);
                self.selected = 0;
            }
            _ => {}
        }
        PickerOutcome::Open
    }

    pub fn draw(&self, frame: &mut Frame, area: Rect) {
        let visible = self.visible();
        let rows = visible.len().clamp(1, VISIBLE_ROWS) as u16;
        // Filter line + items + blank + footer, inside the dialog border.
        let dialog_area = centered(area, Constraint::Length(48), Constraint::Length(rows + 5));
        let title = format!("{} · {}", self.title, self.items.len());
        let inner = dialog(frame, dialog_area, &title);

        let mut lines: Vec<Line> = vec![match (&self.filter, self.loading) {
            (_, true) => Line::styled(" ◌ loading…", theme::muted()),
            (filter, _) if filter.is_empty() => {
                Line::styled(" type to filter…", theme::muted().italic())
            }
            (filter, _) => Line::from_iter([
                Span::styled(" /", Style::default().fg(theme::INDIGO)),
                Span::styled(filter.clone(), Style::default().fg(theme::LAVENDER)),
                Span::styled("█", Style::default().fg(theme::INDIGO)),
            ]),
        }];

        if visible.is_empty() && !self.loading {
            lines.push(Line::styled(" nothing matches", theme::muted().italic()));
        }

        // Scroll just enough to keep the selection in view.
        let top = self.selected.saturating_sub(VISIBLE_ROWS - 1);
        for (index, item) in visible.iter().enumerate().skip(top).take(VISIBLE_ROWS) {
            let selected = index == self.selected;
            let active = self.active.as_deref() == Some(item.as_str());
            let marker = if selected { "› " } else { "  " };
            let dot = if active { "● " } else { "  " };
            let style = if selected {
                Style::default()
                    .fg(theme::LAVENDER)
                    .bg(theme::DEEP)
                    .add_modifier(Modifier::BOLD)
            } else if active {
                Style::default().fg(theme::MINT)
            } else {
                theme::muted()
            };
            lines.push(Line::from_iter([
                Span::styled(marker, Style::default().fg(theme::INDIGO)),
                Span::styled(dot, Style::default().fg(theme::MINT)),
                Span::styled((*item).clone(), style),
            ]));
        }

        lines.push(Line::raw(""));
        lines.push(Line::styled(
            " ↑/↓ move · Enter select · Esc close",
            theme::muted().italic(),
        ));

        frame.render_widget(Paragraph::new(lines), inner);
    }
}
