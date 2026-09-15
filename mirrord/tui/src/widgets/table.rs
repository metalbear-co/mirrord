use std::ops::ControlFlow;

use crossterm::event::{Event, KeyCode};
use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    style::{Modifier, Style},
    text::Span,
    widgets::{Cell, Paragraph, Row, TableState},
};

use crate::helpers::centered;

/// A column of a [`Table`].
pub struct Column {
    /// Text shown in the header row.
    pub name: &'static str,
    /// Width of the column.
    pub width: Constraint,
}

/// A row of a [`Table`].
pub trait TableRow {
    /// Columns rendered for every row.
    const COLUMNS: &'static [Column];

    /// Identifies a row across refreshes.
    type Id: Eq;

    /// Returns the identity of this row.
    fn id(&self) -> Self::Id;

    /// Returns the cells of this row, one per entry in [`Self::COLUMNS`].
    fn cells(&self) -> Vec<Cell<'_>>;
}

/// A table of selectable rows.
///
/// Consumes the events that move the selection and passes every other event through.
pub struct Table<T> {
    rows: Vec<T>,
    state: TableState,
    empty_message: &'static str,
}

impl<T> Table<T> {
    const PAGE: u16 = 10;

    /// Creates an empty table.
    ///
    /// `empty_message` is drawn in place of the rows while the table has none.
    pub fn new(empty_message: &'static str) -> Self {
        Self {
            rows: Vec::new(),
            state: TableState::new(),
            empty_message,
        }
    }

    /// The current rows.
    #[allow(unused, reason = "Nothing uses this yet.")]
    pub fn rows(&self) -> &[T] {
        &self.rows
    }

    /// The selected row, if any.
    #[allow(unused, reason = "Nothing uses this yet.")]
    pub fn selected(&self) -> Option<&T> {
        self.state.selected().and_then(|index| self.rows.get(index))
    }
}

impl<T: TableRow> Table<T> {
    /// Replaces the rows.
    ///
    /// Keeps the selection on the row it was on if that row is still present, and selects the
    /// first row otherwise.
    pub fn set_rows(&mut self, rows: Vec<T>) {
        let selected = self
            .selected()
            .map(TableRow::id)
            .and_then(|id| rows.iter().position(|row| row.id() == id));

        self.rows = rows;

        *self.state.selected_mut() = match selected {
            Some(index) => Some(index),
            None if self.rows.is_empty() => None,
            None => Some(0),
        };
    }

    /// Renders the table into `area`.
    pub fn draw(&mut self, frame: &mut Frame, area: Rect) {
        if self.rows.is_empty() {
            frame.render_widget(
                Paragraph::new(Span::styled(self.empty_message, Style::default().gray()))
                    .centered(),
                centered(area, Constraint::Fill(1), Constraint::Length(1)),
            );

            return;
        }

        let header = Row::new(T::COLUMNS.iter().map(|column| Cell::from(column.name)))
            .style(Style::default().add_modifier(Modifier::BOLD));

        let rows = self.rows.iter().map(|row| Row::new(row.cells()));

        frame.render_stateful_widget(
            ratatui::widgets::Table::new(rows, T::COLUMNS.iter().map(|column| column.width))
                .header(header)
                .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
                .highlight_symbol("> "),
            area,
            &mut self.state,
        );
    }

    /// Moves the selection according to `event`.
    ///
    /// Breaks if the event moved the selection, and passes the event through otherwise.
    pub fn handle_event(&mut self, event: Event) -> ControlFlow<(), Event> {
        let Event::Key(key) = event else {
            return ControlFlow::Continue(event);
        };

        if self.rows.is_empty() {
            return ControlFlow::Continue(event);
        }

        match key.code {
            KeyCode::Down | KeyCode::Char('j') => self.state.select_next(),
            KeyCode::Up | KeyCode::Char('k') => self.state.select_previous(),
            KeyCode::Home => self.state.select_first(),
            KeyCode::End => self.state.select_last(),
            KeyCode::PageDown => self.state.scroll_down_by(Self::PAGE),
            KeyCode::PageUp => self.state.scroll_up_by(Self::PAGE),
            _ => return ControlFlow::Continue(event),
        }

        let last = self.rows.len() - 1;

        *self.state.selected_mut() = Some(self.state.selected().unwrap_or(0).min(last));

        ControlFlow::Break(())
    }
}
