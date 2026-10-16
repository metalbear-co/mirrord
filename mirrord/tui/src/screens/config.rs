//! The Config screen: every option of `.mirrord/mirrord.json`, browsed as a tree and edited in
//! place.
//!
//! Nothing here is written per option. The options, their docs, plans and editors all come from
//! the config schema ([`crate::config_schema`]), so new options appear here without a change to
//! this screen.

use std::ops::ControlFlow;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use mirrord_config::plan::Plan;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use serde_json::Value;

use crate::{
    config_schema::{FIELDS, Field, Shape},
    context::Context,
    helpers::{ellipsize, wrap},
    kit::{
        hints::Hint,
        markdown, panel,
        search::{SearchBox, SearchOutcome},
        text_input::TextInput,
        tree::{TreeNav, TreeRow},
    },
    screens::Screen,
    theme,
    widgets::picker::{Picker, PickerOutcome},
};

pub(crate) mod document;
mod edit;
mod rows;

use document::{Document, SaveError};
use rows::{Filter, OptionPath, Row};

/// Share of the body the option tree takes; the details panel gets the rest.
const TREE_PERCENT: u16 = 55;

/// Most lines the status above the footer grows to before it is cut.
const MAX_STATUS_LINES: usize = 3;

/// Lines moved by `PageUp` / `PageDown` in the details panel.
const DETAILS_PAGE: u16 = 10;

pub struct ConfigScreen {
    document: Document,
    nav: TreeNav<OptionPath>,
    search: SearchBox,
    /// `m`: list only the options that have a value.
    set_only: bool,
    /// The option being typed into, if any.
    editor: Option<Editor>,
    /// A pick-one dialog over the screen, if any.
    picker: Option<(PickerPurpose, Picker)>,
    /// What the last save or check said, shown above the footer.
    status: Status,
    /// First tree row on screen, moved only as far as needed to keep the cursor in view.
    tree_scroll: usize,
    details_scroll: u16,
}

/// A one-line editor open on an option.
struct Editor {
    path: OptionPath,
    input: TextInput,
    /// Why the last `Enter` was refused.
    error: Option<String>,
}

/// What a picked item is for.
enum PickerPurpose {
    /// A value for a [`Shape::Choice`] option.
    Value(OptionPath),
    /// Which shape a [`Shape::Variants`] option takes.
    Variant(OptionPath),
}

#[derive(Default)]
enum Status {
    /// Nothing to report beyond whether the document checks out.
    #[default]
    Idle,
    Saved,
    Problem(String),
}

impl ConfigScreen {
    fn filter(&self) -> Filter {
        Filter {
            phrase: self.search.phrase().to_lowercase(),
            docs: false,
            set_only: self.set_only,
        }
    }

    fn rows(&self) -> Vec<Row> {
        rows::flatten(&FIELDS, &self.document, &self.nav, &self.filter())
    }

    /// The row under the cursor, with the cursor put back on a row that exists first.
    fn focused(&mut self) -> Option<Row> {
        let mut rows = self.rows();
        let index = self.nav.reconcile(&rows)?;
        (index < rows.len()).then(|| rows.swap_remove(index))
    }

    /// `Enter`: what editing an option means depends on its shape.
    fn edit(&mut self, row: &Row) {
        let value = self.document.get(&row.path.0).cloned();

        // A section folds; an option that takes several forms is edited, and folds with the arrows.
        if matches!(row.field.shape, Shape::Group(_)) {
            let rows = self.rows();
            if self.nav.is_collapsed(&row.path) {
                self.nav.expand_or_down(&rows);
            } else {
                self.nav.collapse_or_up(&rows);
            }
            return;
        }

        let shape = match (&row.field.shape, &value) {
            (shape @ Shape::Variants(_), Some(value)) => {
                shape.active_variant(value).map(|v| &v.shape)
            }
            (Shape::Variants(_), None) => {
                self.pick_variant(row);
                return;
            }
            (shape, _) => Some(shape),
        };

        match shape {
            Some(Shape::Toggle) => self.toggle(row),
            Some(Shape::Choice(options)) => {
                let current = value.as_ref().and_then(Value::as_str).map(str::to_owned);
                self.picker = Some((
                    PickerPurpose::Value(row.path.clone()),
                    Picker::new("Choose a value", options.clone(), current),
                ));
            }
            Some(shape) => self.open_editor(row.path.clone(), edit::to_text(shape, value.as_ref())),
            // The value matches none of the shapes the option takes - typed in by hand, most
            // likely. JSON still edits it.
            None => self.open_editor(
                row.path.clone(),
                value.map(|value| value.to_string()).unwrap_or_default(),
            ),
        }
    }

    fn open_editor(&mut self, path: OptionPath, text: String) {
        let mut input = TextInput::default();
        input.paste(&text);
        self.editor = Some(Editor {
            path,
            input,
            error: None,
        });
    }

    /// `Space`: flips an on/off option. An unset one is off by being unset, so it turns on.
    fn toggle(&mut self, row: &Row) {
        let on = self
            .document
            .get(&row.path.0)
            .and_then(Value::as_bool)
            .unwrap_or(false);
        self.document.set(&row.path.0, Value::Bool(!on));
        self.status = Status::Idle;
    }

    /// `v`: lets the user pick which of its shapes a [`Shape::Variants`] option takes.
    fn pick_variant(&mut self, row: &Row) {
        let Shape::Variants(variants) = &row.field.shape else {
            return;
        };
        let current = self
            .document
            .get(&row.path.0)
            .and_then(|value| row.field.shape.active_variant(value))
            .map(|variant| variant.label.clone());
        let labels = variants
            .iter()
            .map(|variant| variant.label.clone())
            .collect();
        self.picker = Some((
            PickerPurpose::Variant(row.path.clone()),
            Picker::new("Choose a form", labels, current),
        ));
    }

    /// Switches an option to the variant labelled `label`, starting it from a value of that shape.
    fn apply_variant(&mut self, path: OptionPath, label: &str) {
        let Some(field) = field_at(&path) else {
            return;
        };
        let Shape::Variants(variants) = &field.shape else {
            return;
        };
        let Some(variant) = variants.iter().find(|variant| variant.label == label) else {
            return;
        };

        match &variant.shape {
            Shape::Toggle => self.document.set(&path.0, Value::Bool(true)),
            Shape::Choice(options) => {
                let Some(first) = options.first() else {
                    return;
                };
                self.document.set(&path.0, Value::String(first.clone()));
            }
            Shape::Group(_) | Shape::Map(_) => self
                .document
                .set(&path.0, Value::Object(Default::default())),
            // Anything else needs typing; the editor writes it on `Enter`.
            _ => self.open_editor(path, String::new()),
        }
        self.status = Status::Idle;
    }

    fn commit_editor(&mut self) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        let Some(field) = field_at(&editor.path) else {
            self.editor = None;
            return;
        };

        // A variant option is read with the shape its current value has, or the first shape that
        // accepts what was typed.
        let shape = match &field.shape {
            Shape::Variants(variants) => {
                let current = self.document.get(&editor.path.0);
                let preferred = current.and_then(|value| field.shape.active_variant(value));
                let candidates = preferred.into_iter().chain(variants.iter());
                let parsed = candidates
                    .map(|variant| edit::from_text(&variant.shape, editor.input.value()))
                    .find(Result::is_ok);
                match parsed {
                    Some(Ok(value)) => {
                        self.finish_edit(value);
                        return;
                    }
                    _ => {
                        editor.error = Some(format!("expected {}", field.shape.describe()));
                        return;
                    }
                }
            }
            shape => shape,
        };

        match edit::from_text(shape, editor.input.value()) {
            Ok(value) => self.finish_edit(value),
            Err(error) => editor.error = Some(error),
        }
    }

    fn finish_edit(&mut self, value: Option<Value>) {
        let Some(editor) = self.editor.take() else {
            return;
        };
        match value {
            Some(value) => self.document.set(&editor.path.0, value),
            None => self.document.unset(&editor.path.0),
        }
        self.status = Status::Idle;
    }

    /// Saves the file and reports the outcome on the status line. Returns the outcome too, for
    /// the quit dialog: a file that saved but was rejected is still saved.
    fn save(&mut self) -> Result<(), SaveError> {
        let result = self.document.save();
        self.status = match &result {
            Ok(()) => Status::Saved,
            Err(SaveError::Unreadable(reason)) => Status::Problem(format!(
                "Not saved: the file could not be read ({reason}), and saving would replace it"
            )),
            Err(SaveError::Write(error)) => Status::Problem(format!("Not saved: {error}")),
            Err(SaveError::Rejected(error)) => {
                Status::Problem(format!("Saved, but mirrord rejects it: {error}"))
            }
        };
        result
    }

    fn handle_picker(&mut self, key: KeyEvent) {
        let Some((_, picker)) = &mut self.picker else {
            return;
        };
        match picker.handle_key(key) {
            PickerOutcome::Open => {}
            PickerOutcome::Cancelled => self.picker = None,
            PickerOutcome::Picked(choice) => match self.picker.take() {
                Some((PickerPurpose::Value(path), _)) => {
                    self.document.set(&path.0, Value::String(choice));
                    self.status = Status::Idle;
                }
                Some((PickerPurpose::Variant(path), _)) => self.apply_variant(path, &choice),
                None => {}
            },
        }
    }

    fn handle_editor(&mut self, event: &Event) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        match event {
            Event::Paste(text) => editor.input.paste(text),
            Event::Key(key) => match key.code {
                KeyCode::Enter => self.commit_editor(),
                KeyCode::Esc => self.editor = None,
                _ => {
                    if editor.input.handle_key(key) {
                        editor.error = None;
                    }
                }
            },
            _ => {}
        }
    }

    fn draw_tree(&mut self, frame: &mut Frame, area: Rect, rows: &[Row], selected: Option<usize>) {
        if rows.is_empty() {
            let message = if self.search.phrase().is_empty() {
                "No options have a value yet. Press m to list them all."
            } else {
                "No options match the search"
            };
            crate::kit::state_view::draw_message(frame, area, message, theme::muted());
            return;
        }

        let height = usize::from(area.height);
        if let Some(selected) = selected {
            if selected < self.tree_scroll {
                self.tree_scroll = selected;
            } else if selected >= self.tree_scroll + height {
                self.tree_scroll = selected + 1 - height;
            }
        }
        self.tree_scroll = self.tree_scroll.min(rows.len().saturating_sub(height));

        let width = usize::from(area.width);
        // The key column takes a little more than half; the value gets the rest.
        let key_width = width * 3 / 5;

        let lines: Vec<Line> = rows
            .iter()
            .enumerate()
            .skip(self.tree_scroll)
            .take(height)
            .map(|(index, row)| {
                let value = self.document.get(&row.path.0);
                tree_line(row, value, key_width, width, Some(index) == selected)
            })
            .collect();

        frame.render_widget(Paragraph::new(lines), area);
    }

    fn draw_details(&mut self, frame: &mut Frame, area: Rect, row: Option<&Row>) {
        let Some(row) = row else {
            return;
        };
        let width = usize::from(area.width.saturating_sub(1)).max(1);
        let value = self.document.get(&row.path.0);
        let mut lines = vec![Line::styled(row.path.dotted(), theme::title())];

        if let Some(editor) = self
            .editor
            .as_ref()
            .filter(|editor| editor.path == row.path)
        {
            let mut spans = vec![Span::styled("edit  ", theme::warning())];
            spans.extend(editor.input.spans(
                Style::default().fg(theme::LAVENDER),
                theme::title(),
                true,
            ));
            lines.push(Line::raw(""));
            lines.push(Line::from(spans));
            match &editor.error {
                Some(error) => lines.extend(
                    wrap(error, width)
                        .into_iter()
                        .map(|line| Line::styled(line, theme::error())),
                ),
                None => lines.push(Line::styled(
                    "Enter keep · Esc cancel · empty to unset",
                    theme::muted(),
                )),
            }
        }

        lines.push(Line::raw(""));
        lines.push(Line::from_iter([
            Span::styled("takes  ", theme::muted()),
            Span::raw(row.field.shape.describe()),
        ]));
        if let Some(plan) = row.field.plan.filter(|plan| *plan > Plan::Oss) {
            lines.push(Line::styled(plan_requirement(plan), theme::warning()));
        }

        match value {
            Some(value) => {
                lines.push(Line::styled("value", theme::muted()));
                let pretty = serde_json::to_string_pretty(value).unwrap_or_default();
                lines.extend(pretty.lines().map(|line| {
                    Line::styled(format!("  {line}"), Style::default().fg(theme::LAVENDER))
                }));
            }
            None => lines.push(Line::styled(
                "not set - mirrord's default applies",
                theme::muted(),
            )),
        }

        if let Some(doc) = &row.field.doc {
            lines.push(Line::raw(""));
            lines.extend(markdown::render(doc, width));
        }

        let max_scroll =
            u16::try_from(lines.len().saturating_sub(usize::from(area.height))).unwrap_or(u16::MAX);
        self.details_scroll = self.details_scroll.min(max_scroll);
        frame.render_widget(Paragraph::new(lines).scroll((self.details_scroll, 0)), area);
    }

    /// What the document's state says, and how to color it: whether it checks out, and what
    /// the last save found.
    fn status(&self) -> (String, Style) {
        if let Some(reason) = self.document.unreadable() {
            return (
                format!(
                    "✗ {} could not be read ({reason}). Saving is off so the file is not replaced.",
                    self.document.display_path()
                ),
                theme::error(),
            );
        }
        match (&self.status, self.document.check()) {
            (Status::Problem(problem), _) => (format!("✗ {problem}"), theme::error()),
            (_, Err(error)) => (format!("✗ {error}"), theme::error()),
            (Status::Saved, Ok(())) => (
                format!(
                    "✓ saved to {} and accepted by mirrord",
                    self.document.display_path()
                ),
                Style::default().fg(theme::MINT),
            ),
            (Status::Idle, Ok(())) if self.document.has_comments() => (
                format!(
                    "✓ valid · saving rewrites {} without its comments",
                    self.document.display_path()
                ),
                theme::warning(),
            ),
            (Status::Idle, Ok(())) => ("✓ valid".to_owned(), Style::default().fg(theme::MINT)),
        }
    }
}

impl Screen for ConfigScreen {
    fn new(_context: Context) -> Self {
        let dir = std::env::current_dir().unwrap_or_default();

        // Folded to the top-level options, so the first view is short and every section opens on
        // request.
        let mut nav = TreeNav::default();
        let mut sections = Vec::new();
        rows::section_paths(&FIELDS, &[], &mut sections);
        for section in sections {
            nav.collapse(section);
        }

        Self {
            document: Document::load(&dir),
            nav,
            search: SearchBox::default(),
            set_only: false,
            editor: None,
            picker: None,
            status: Status::Idle,
            tree_scroll: 0,
            details_scroll: 0,
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let search_height = u16::from(self.search.visible());
        // A rejection from mirrord is the one message worth reading in full, so the status wraps,
        // up to a few lines.
        let (status_text, status_style) = self.status();
        let status_lines = wrap(&status_text, usize::from(area.width).max(1));
        let status_height =
            u16::try_from(status_lines.len().clamp(1, MAX_STATUS_LINES)).unwrap_or(1);
        let [body, search, status] = Layout::vertical([
            Constraint::Min(3),
            Constraint::Length(search_height),
            Constraint::Length(status_height),
        ])
        .areas(area);

        let note = if self.document.is_dirty() {
            format!("{} · unsaved", self.document.display_path())
        } else {
            self.document.display_path()
        };
        let set = self.document.set_count().to_string();
        let block = panel::block("config", Some(&set), Some(&note), None);
        let inner = block.inner(body);
        frame.render_widget(block, body);

        let [tree_area, details_area] = Layout::horizontal([
            Constraint::Percentage(TREE_PERCENT),
            Constraint::Percentage(100 - TREE_PERCENT),
        ])
        .spacing(2)
        .areas(inner);

        let rows = self.rows();
        let selected = self.nav.reconcile(&rows);
        self.draw_tree(frame, tree_area, &rows, selected);
        let focused = selected.and_then(|index| rows.get(index));
        self.draw_details(frame, details_area, focused);

        if search_height > 0 {
            self.search.draw(frame, search);
        }
        frame.render_widget(
            Paragraph::new(
                status_lines
                    .into_iter()
                    .map(|line| Line::styled(line, status_style))
                    .collect::<Vec<_>>(),
            ),
            status,
        );

        if let Some((_, picker)) = &self.picker {
            picker.draw(frame, body);
        }
    }

    fn handle_event(&mut self, event: Event) -> ControlFlow<(), Event> {
        if self.picker.is_some() {
            if let Event::Key(key) = event {
                self.handle_picker(key);
            }
            return ControlFlow::Break(());
        }
        if self.editor.is_some() {
            self.handle_editor(&event);
            return ControlFlow::Break(());
        }

        match self.search.handle_event(&event) {
            SearchOutcome::Ignored => {}
            SearchOutcome::Consumed | SearchOutcome::Changed => return ControlFlow::Break(()),
        }

        let Event::Key(key) = &event else {
            return ControlFlow::Continue(event);
        };

        let rows = self.rows();
        match key.code {
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let _ = self.save();
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.nav.move_by(&rows, 1);
                self.details_scroll = 0;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.nav.move_by(&rows, -1);
                self.details_scroll = 0;
            }
            KeyCode::Home => self.nav.move_by(&rows, isize::MIN),
            KeyCode::End => self.nav.move_by(&rows, isize::MAX),
            KeyCode::Left | KeyCode::Char('h') => self.nav.collapse_or_up(&rows),
            KeyCode::Right | KeyCode::Char('l') => self.nav.expand_or_down(&rows),
            KeyCode::PageDown => {
                self.details_scroll = self.details_scroll.saturating_add(DETAILS_PAGE)
            }
            KeyCode::PageUp => {
                self.details_scroll = self.details_scroll.saturating_sub(DETAILS_PAGE)
            }
            KeyCode::Char('m') => self.set_only = !self.set_only,
            KeyCode::Enter => {
                if let Some(row) = self.focused() {
                    self.edit(&row);
                }
            }
            KeyCode::Char(' ') => {
                if let Some(row) = self.focused()
                    && matches!(row.field.shape, Shape::Toggle)
                {
                    self.toggle(&row);
                }
            }
            KeyCode::Char('v') => {
                if let Some(row) = self.focused() {
                    self.pick_variant(&row);
                }
            }
            KeyCode::Char('d') | KeyCode::Delete => {
                if let Some(row) = self.focused() {
                    self.document.unset(&row.path.0);
                    self.status = Status::Idle;
                }
            }
            _ => return ControlFlow::Continue(event),
        }

        ControlFlow::Break(())
    }

    fn unsaved_changes(&self) -> Option<String> {
        self.document.is_dirty().then(|| {
            format!(
                "Config: edits to {} are not saved.",
                self.document.display_path()
            )
        })
    }

    fn save_changes(&mut self) -> Result<(), String> {
        match self.save() {
            Ok(()) | Err(SaveError::Rejected(_)) => Ok(()),
            Err(SaveError::Unreadable(reason)) => {
                Err(format!("the file could not be read ({reason})"))
            }
            Err(SaveError::Write(error)) => Err(error),
        }
    }

    fn hints(&self) -> Vec<Hint> {
        if self.editor.is_some() {
            return vec![
                Hint::new("enter", "keep"),
                Hint::new("esc", "cancel"),
                Hint::new("empty", "unset"),
            ];
        }
        if self.search.is_editing() {
            return self.search.hints();
        }

        let mut hints = vec![
            Hint::new("\u{2191}/\u{2193}", "move"),
            Hint::new("\u{2190}/\u{2192}", "fold"),
            Hint::new("enter", "edit"),
            Hint::new("v", "form"),
            Hint::new("d", "unset"),
        ];
        hints.extend(self.search.hints());
        hints.push(Hint::new(
            "m",
            if self.set_only {
                "all options"
            } else {
                "set only"
            },
        ));
        hints.push(Hint::new("ctrl-s", "save"));
        hints.push(Hint::new("pgup/pgdn", "scroll docs"));
        hints
    }
}

/// The field at `path`, looked up through sections and settings variants.
fn field_at(path: &OptionPath) -> Option<&'static Field> {
    let mut fields: &'static [Field] = &FIELDS;
    let mut found = None;
    for key in &path.0 {
        let field = fields.iter().find(|field| &field.key == key)?;
        fields = match &field.shape {
            Shape::Group(children) => children,
            Shape::Variants(variants) => variants
                .iter()
                .find_map(|variant| match &variant.shape {
                    Shape::Group(children) => Some(children.as_slice()),
                    _ => None,
                })
                .unwrap_or_default(),
            _ => &[],
        };
        found = Some(field);
    }
    found
}

fn plan_requirement(plan: Plan) -> &'static str {
    match plan {
        Plan::Oss => "",
        Plan::Team => "needs mirrord for Teams",
        Plan::Enterprise => "needs mirrord Enterprise",
    }
}

/// One tree row: indent, fold marker, key and plan badge on the left, the value on the right.
fn tree_line(
    row: &Row,
    value: Option<&Value>,
    key_width: usize,
    width: usize,
    selected: bool,
) -> Line<'static> {
    let marker = match row.fold() {
        crate::kit::tree::Fold::Leaf => "  ",
        crate::kit::tree::Fold::Expanded => "\u{25be} ",
        crate::kit::tree::Fold::Collapsed => "\u{25b8} ",
    };
    let badge = match row.field.plan {
        Some(Plan::Team) => " team",
        Some(Plan::Enterprise) => " enterprise",
        _ => "",
    };
    let indent = "  ".repeat(row.depth());

    // One column is always left between the key and the value.
    let key_room = key_width.saturating_sub(
        indent.chars().count() + marker.chars().count() + badge.chars().count() + 1,
    );
    let key = ellipsize(&row.field.key, key_room);
    let used = indent.chars().count()
        + marker.chars().count()
        + key.chars().count()
        + badge.chars().count();
    let padding = " ".repeat(key_width.saturating_sub(used));

    let (value_text, value_style) = match value {
        Some(Value::Object(object)) if row.fold() != crate::kit::tree::Fold::Leaf => {
            (format!("{} set", object.len()), theme::muted())
        }
        Some(value) => (compact(value), Style::default().fg(theme::LAVENDER)),
        None => ("-".to_owned(), theme::muted()),
    };
    let value_text = ellipsize(&value_text, width.saturating_sub(key_width));

    let key_style = if value.is_some() {
        Style::default()
            .fg(theme::LAVENDER)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };

    let mut line = Line::from_iter([
        Span::styled(format!("{indent}{marker}"), theme::muted()),
        Span::styled(key, key_style),
        Span::styled(badge, theme::warning()),
        Span::raw(padding),
        Span::styled(value_text, value_style),
    ]);
    if selected {
        line = line.style(theme::selection());
        let filled: usize = line.width();
        line.push_span(Span::raw(" ".repeat(width.saturating_sub(filled))));
    }
    line
}

/// A value on one line, strings without their quotes.
fn compact(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        value => value.to_string(),
    }
}
