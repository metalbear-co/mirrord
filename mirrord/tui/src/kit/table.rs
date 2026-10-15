//! A table of rows the user moves a selection through.
//!
//! A screen describes its columns once, as an enum implementing [`Columns`], and [`DataTable`]
//! draws the header, the rows, the selection and the cut-off marks, and moves the selection on
//! the usual keys.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::Style,
    text::Span,
    widgets::{Cell, Row, Table, TableState},
};

use crate::{helpers::ellipsize, kit::hints::Hint, theme};

/// Columns between two cells.
const COLUMN_SPACING: u16 = 1;

/// Most room a single runtime column asks for, so one very long value cannot squeeze the rest.
const MAX_WEIGHT: usize = 48;

/// Rows moved by `PageUp` / `PageDown`.
const PAGE: u16 = 10;

/// The columns of a table, usually an enum with one variant per column.
pub trait Columns: Copy + 'static {
    /// What one row is drawn from.
    type Row;

    /// Every column, in the order they are drawn.
    const ALL: &'static [Self];

    /// The header cell.
    fn title(self) -> &'static str;

    /// How wide the column is.
    fn constraint(self) -> Constraint;

    /// The text of this column for `row`, and how to color it.
    fn value(self, row: &Self::Row) -> (String, Style);
}

#[derive(Default)]
pub struct DataTable {
    state: TableState,
}

impl DataTable {
    /// Index of the selected row among the rows last drawn.
    pub fn selected(&self) -> Option<usize> {
        self.state.selected()
    }

    /// Moves the selection back to the first row, for when the rows under it changed wholesale
    /// (a new search phrase).
    pub fn select_first(&mut self) {
        self.state.select_first();
    }

    /// Draws `rows` with the header above them.
    ///
    /// The rows can shrink between frames (sessions end, a search narrows them), so the selection
    /// is clamped to the last row here, and put on the first row when there was none.
    pub fn draw<C: Columns>(&mut self, frame: &mut Frame, area: Rect, rows: &[&C::Row]) {
        match self.state.selected() {
            Some(selected) if selected >= rows.len() => {
                self.state.select(rows.len().checked_sub(1));
            }
            Some(_) => {}
            None if rows.is_empty() => {}
            None => self.state.select(Some(0)),
        }

        // The table cuts off whatever does not fit without a trace, so cells are cut here instead,
        // against the very widths it lays out, ending in `…` so a cut value never passes for the
        // whole one.
        let widths = Layout::horizontal(C::ALL.iter().map(|column| column.constraint()))
            .spacing(COLUMN_SPACING)
            .split(Rect { height: 1, ..area });
        let columns = || {
            C::ALL
                .iter()
                .zip(widths.iter().map(|rect| usize::from(rect.width)))
        };

        let header = Row::new(columns().map(|(column, width)| ellipsize(column.title(), width)))
            .style(theme::table_header());
        let body = rows.iter().map(|row| {
            Row::new(columns().map(|(column, width)| {
                let (value, style) = column.value(row);
                Cell::from(Span::styled(ellipsize(&value, width), style))
            }))
        });

        frame.render_stateful_widget(
            Table::new(body, C::ALL.iter().map(|column| column.constraint()))
                .column_spacing(COLUMN_SPACING)
                .header(header)
                .row_highlight_style(theme::selection()),
            area,
            &mut self.state,
        );
    }

    /// Draws rows whose columns are only known at runtime, such as the columns the API server
    /// reports for a resource type. Each column gets room in proportion to the longest value it
    /// holds, so a column of names is not cut short to make room for a column of `True`s.
    pub fn draw_dynamic(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        header: &[String],
        rows: &[Vec<String>],
    ) {
        match self.state.selected() {
            Some(selected) if selected >= rows.len() => {
                self.state.select(rows.len().checked_sub(1))
            }
            Some(_) => {}
            None if rows.is_empty() => {}
            None => self.state.select(Some(0)),
        }

        let constraints: Vec<Constraint> = (0..header.len())
            .map(|index| {
                let longest = rows
                    .iter()
                    .filter_map(|row| row.get(index))
                    .chain(header.get(index))
                    .map(|cell| cell.chars().count())
                    .max()
                    .unwrap_or_default();
                Constraint::Fill(u16::try_from(longest.clamp(4, MAX_WEIGHT)).unwrap_or(4))
            })
            .collect();
        let widths = Layout::horizontal(constraints.clone())
            .spacing(COLUMN_SPACING)
            .split(Rect { height: 1, ..area });
        let width = |index: usize| widths.get(index).map_or(0, |rect| usize::from(rect.width));

        let header_row = Row::new(
            header
                .iter()
                .enumerate()
                .map(|(index, title)| ellipsize(&title.to_uppercase(), width(index))),
        )
        .style(theme::table_header());
        let body = rows.iter().map(|row| {
            Row::new(row.iter().enumerate().map(|(index, cell)| {
                let style = if index == 0 {
                    Style::default().fg(theme::LAVENDER)
                } else {
                    Style::default()
                };
                Cell::from(Span::styled(ellipsize(cell, width(index)), style))
            }))
        });

        frame.render_stateful_widget(
            Table::new(body, constraints)
                .column_spacing(COLUMN_SPACING)
                .header(header_row)
                .row_highlight_style(theme::selection()),
            area,
            &mut self.state,
        );
    }

    /// Moves the selection. Returns `false` for keys that do not move it.
    ///
    /// Keys handled, arrows and vim keys alike:
    /// - `Down` / `j`, `Up` / `k`: next / previous row
    /// - `Home` / `g`, `End` / `G`: first / last row
    /// - `PageDown` / `PageUp`: 10 rows at a time
    pub fn handle_key(&mut self, key: &KeyEvent) -> bool {
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => self.state.select_next(),
            KeyCode::Up | KeyCode::Char('k') => self.state.select_previous(),
            KeyCode::Home | KeyCode::Char('g') => self.state.select_first(),
            KeyCode::End | KeyCode::Char('G') => self.state.select_last(),
            KeyCode::PageDown => self.state.scroll_down_by(PAGE),
            KeyCode::PageUp => self.state.scroll_up_by(PAGE),
            _ => return false,
        }
        true
    }

    /// The keys [`DataTable::handle_key`] answers to, as the hint line shows them.
    pub fn hints() -> Vec<Hint> {
        vec![Hint::new("\u{2191}/\u{2193}", "move")]
    }
}
