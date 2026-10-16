//! Generic settings form used by the service dialog and the export dialog.
//!
//! Every field is one entry in a registry (`&[SettingDef<T>]`), so adding a
//! setting later - env overrides, ports mapping, anything the config schema
//! grows - is one model field plus one registry entry, no new UI code. The
//! registry is also the seam for generating entries from `mirrord-schema.json`
//! once the wizard goes fully dynamic.
//!
//! The form is assembled from the shared kit, so it edits and picks the way every other view
//! does: text fields edit through [`TextInput`] (`Ctrl+A` selects everything, `Shift` with the
//! arrows selects, a click places the cursor, a drag selects and a double-click picks a word),
//! and fields with a few fixed options draw the same `(●)` / `( )` pick column as the pick lists,
//! with a click on an option picking it.

use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Position, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};
use strum::VariantArray;

use crate::{
    helpers::dialog,
    kit::{
        hints::{self, Hint},
        select_list::{DOUBLE_CLICK, Picks, pick_mark},
        text_input::TextInput,
    },
    screens::targets::{
        history, keys,
        model::{HttpFilterSpec, RunSpec, ServiceEntry, ServiceMode, ServiceSpec, TargetSpec},
        suggest,
    },
    theme,
};

/// One form field: how it renders and how it reads/writes the draft.
pub struct SettingDef<T> {
    pub label: &'static str,
    pub help: &'static str,
    /// Some settings only make sense in some drafts (e.g. the HTTP filter
    /// only applies in split mode).
    pub visible: fn(&T) -> bool,
    /// Suggested values for a text field, derived from the draft; Tab
    /// cycles through them while editing.
    pub suggest: Option<fn(&T) -> Vec<String>>,
    pub widget: WidgetKind<T>,
}

pub enum WidgetKind<T> {
    /// Free text, edited inline. `set` may reject the input with a message.
    Text {
        get: fn(&T) -> String,
        set: fn(&mut T, &str) -> Result<(), String>,
    },
    /// One of a fixed set of options, all on view with the picked one marked; arrows, `Space`
    /// or a click pick.
    Select {
        options: &'static [&'static str],
        get: fn(&T) -> usize,
        set: fn(&mut T, usize),
    },
}

/// How many completion candidates the footer row shows before collapsing
/// the rest into a `(+N more)` note; keeps the row to a glance while the
/// full set stays reachable by typing more of the name.
const MAX_SHOWN_CANDIDATES: usize = 8;

/// Columns before a field's value: the `› ` cursor marker and the label padded to 14.
const LABEL_WIDTH: u16 = 16;

/// Columns before the first completion candidate: `  ↹ `.
const CANDIDATES_PREFIX: u16 = 4;

/// The text field being edited.
struct Editor {
    text: TextInput,
    /// Next suggestion or completion Tab puts in; any edit starts the cycle over.
    suggestion: usize,
}

impl Editor {
    /// Opens on `value` with the cursor at its end.
    fn new(value: &str) -> Self {
        let mut text = TextInput::default();
        text.paste(value);
        Self {
            text,
            suggestion: 0,
        }
    }

    /// Replaces everything from byte `start` on with `replacement`, leaving the cursor at the end.
    fn replace_from(&mut self, start: usize, replacement: &str) {
        let mut value = self
            .text
            .value()
            .get(..start)
            .unwrap_or_default()
            .to_owned();
        value.push_str(replacement);
        self.text.clear();
        self.text.paste(&value);
    }

    /// Replaces the value with the next suggestion, wrapping around.
    fn cycle_suggestion(&mut self, suggestions: &[String]) {
        let Some(suggestion) = self
            .suggestion
            .checked_rem(suggestions.len())
            .and_then(|at| suggestions.get(at))
        else {
            return;
        };
        self.suggestion += 1;
        self.replace_from(0, suggestion);
    }

    /// The token being completed: everything after the last space, and the byte it starts at.
    fn last_token(&self) -> (usize, &str) {
        let value = self.text.value();
        let start = value.rfind(' ').map(|at| at + 1).unwrap_or(0);
        (start, value.get(start..).unwrap_or_default())
    }

    /// Completes the last token against the filesystem, shell-style:
    /// first to the longest common prefix of the matches, then cycling
    /// through them. Bare words complete against the working directory.
    /// Returns `false` when Tab should fall back to the field's
    /// suggestions instead.
    fn complete_path(&mut self) -> bool {
        let (start, token) = self.last_token();
        let Some(target) = completion_target(token) else {
            return false;
        };

        let candidates = suggest::complete_path(&target);
        if candidates.is_empty() {
            // A bare word with no file match falls back to the field's
            // suggestions; an explicit path just has nothing to offer.
            return is_path_like(token);
        }

        let prefix = suggest::common_prefix(&candidates);
        let replacement = if prefix.len() > target.len() {
            self.suggestion = 0;
            prefix
        } else if let Some(pick) = self
            .suggestion
            .checked_rem(candidates.len())
            .and_then(|at| candidates.get(at))
        {
            self.suggestion += 1;
            pick.clone()
        } else {
            return true;
        };

        self.replace_from(start, &replacement);
        true
    }
}

/// What a key press or click in the form resulted in.
pub enum FormOutcome {
    Consumed,
    /// Esc outside of text editing.
    Cancelled,
    /// The finish row was activated and validation passed; the draft is ready.
    Finished,
}

/// Where the form's clickable parts landed in the last frame, for the mouse.
#[derive(Default)]
struct Hits {
    area: Rect,
    /// Each drawn row's line, what it is (an index into the visible fields, or one past the last
    /// for the finish row, like the cursor), and how many characters of its value are scrolled
    /// out on the left.
    rows: Vec<(u16, usize, usize)>,
    /// Column where the values start.
    value_x: u16,
    /// Each option of the select fields: where it is drawn, its field and its index.
    options: Vec<(Rect, usize, usize)>,
    candidates: Vec<(Rect, String)>,
}

pub struct SettingsForm<T: 'static> {
    pub draft: T,
    settings: &'static [SettingDef<T>],
    title: &'static str,
    finish_label: &'static str,
    validate: fn(&T) -> Result<(), String>,
    /// Index into the *visible* fields; one past the end is the finish row.
    cursor: usize,
    editor: Option<Editor>,
    error: Option<String>,
    hits: Hits,
    /// When and on which row the last click landed, to tell a double-click.
    last_click: Option<(Instant, usize)>,
}

impl<T> SettingsForm<T> {
    pub fn new(
        draft: T,
        settings: &'static [SettingDef<T>],
        title: &'static str,
        finish_label: &'static str,
        validate: fn(&T) -> Result<(), String>,
    ) -> Self {
        Self {
            draft,
            settings,
            title,
            finish_label,
            validate,
            cursor: 0,
            editor: None,
            error: None,
            hits: Hits::default(),
            last_click: None,
        }
    }

    pub fn set_error(&mut self, error: String) {
        self.error = Some(error);
    }

    /// Moves the cursor to the visible field with this label and starts
    /// editing it, so a caller can drop the user straight into fixing one
    /// specific field.
    pub fn edit_field(&mut self, label: &str) {
        let visible = self.visible();
        let Some(index) = visible.iter().position(|def| def.label == label) else {
            return;
        };
        self.cursor = index;
        let Some(def) = visible.get(index) else {
            return;
        };
        if let WidgetKind::Text { get, .. } = &def.widget {
            self.editor = Some(Editor::new(&get(&self.draft)));
        }
    }

    fn visible(&self) -> Vec<&'static SettingDef<T>> {
        self.settings
            .iter()
            .filter(|def| (def.visible)(&self.draft))
            .collect()
    }

    /// Routes a terminal paste into the field being edited; `false` when
    /// no text editor is open to take it. The ends are trimmed: the trailing
    /// newline most clipboards carry must not end up in a one-line value.
    pub fn paste(&mut self, text: &str) -> bool {
        let Some(editor) = &mut self.editor else {
            return false;
        };
        editor.suggestion = 0;
        editor.text.paste(text.trim());
        true
    }

    /// Writes the edited value into the draft and closes the editor; a value the field rejects
    /// shows why.
    fn apply_edit(&mut self) {
        let Some(editor) = self.editor.take() else {
            return;
        };
        if let Some(def) = self.visible().get(self.cursor)
            && let WidgetKind::Text { set, .. } = &def.widget
        {
            self.error = set(&mut self.draft, editor.text.value().trim()).err();
        }
    }

    fn finish(&mut self) -> FormOutcome {
        match (self.validate)(&self.draft) {
            Ok(()) => FormOutcome::Finished,
            Err(message) => {
                self.error = Some(message);
                FormOutcome::Consumed
            }
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> FormOutcome {
        let visible = self.visible();

        if let Some(editor) = &mut self.editor {
            match key.code {
                KeyCode::Esc => self.editor = None,
                KeyCode::Tab => {
                    // A path-like token completes against the filesystem;
                    // anything else cycles the field's suggestions.
                    if !editor.complete_path()
                        && let Some(suggest) = visible.get(self.cursor).and_then(|def| def.suggest)
                    {
                        editor.cycle_suggestion(&suggest(&self.draft));
                    }
                }
                KeyCode::Enter => self.apply_edit(),
                _ => {
                    editor.suggestion = 0;
                    editor.text.handle_key(&key);
                }
            }
            return FormOutcome::Consumed;
        }

        // The selecting keys on a focused text field open its editor and
        // apply at once, so the field behaves as if always editable
        // instead of demanding an Enter first.
        if let Some(def) = visible.get(self.cursor)
            && let WidgetKind::Text { get, .. } = &def.widget
        {
            let shift_move = key.modifiers.contains(KeyModifiers::SHIFT)
                && matches!(
                    key.code,
                    KeyCode::Left | KeyCode::Right | KeyCode::Home | KeyCode::End
                );
            let select_all =
                key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('a');
            if shift_move || select_all {
                let mut editor = Editor::new(&get(&self.draft));
                editor.text.handle_key(&key);
                self.editor = Some(editor);
                return FormOutcome::Consumed;
            }
        }

        match key.code {
            KeyCode::Esc => return FormOutcome::Cancelled,
            KeyCode::Up | KeyCode::Char(keys::UP) => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char(keys::DOWN) => {
                self.cursor = (self.cursor + 1).min(visible.len());
            }
            KeyCode::Enter if self.cursor == visible.len() => return self.finish(),
            KeyCode::Enter | KeyCode::Char(' ') => {
                match visible.get(self.cursor).map(|def| &def.widget) {
                    Some(WidgetKind::Text { get, .. }) if key.code == KeyCode::Enter => {
                        self.editor = Some(Editor::new(&get(&self.draft)));
                    }
                    Some(WidgetKind::Select { options, get, set }) => {
                        let next = (get(&self.draft) + 1) % options.len();
                        set(&mut self.draft, next);
                    }
                    _ => {}
                }
            }
            KeyCode::Left | KeyCode::Right => {
                let Some(def) = visible.get(self.cursor) else {
                    return FormOutcome::Consumed;
                };
                match &def.widget {
                    WidgetKind::Select { options, get, set } => {
                        let current = get(&self.draft);
                        let next = match key.code {
                            KeyCode::Right => (current + 1) % options.len(),
                            _ => (current + options.len() - 1) % options.len(),
                        };
                        set(&mut self.draft, next);
                    }
                    // A text field with suggestions cycles them like a
                    // select while it is not being edited; inside the
                    // editor the arrows stay cursor movement.
                    WidgetKind::Text { get, set } => {
                        let suggestions = def
                            .suggest
                            .map(|suggest| suggest(&self.draft))
                            .unwrap_or_default();
                        let Some(last) = suggestions.len().checked_sub(1) else {
                            return FormOutcome::Consumed;
                        };
                        let current = get(&self.draft);
                        let position = suggestions.iter().position(|s| *s == current);
                        let next = match (key.code, position) {
                            (KeyCode::Right, Some(at)) => (at + 1) % suggestions.len(),
                            (KeyCode::Right, None) => 0,
                            (_, Some(at)) => (at + suggestions.len() - 1) % suggestions.len(),
                            (_, None) => last,
                        };
                        if let Some(pick) = suggestions.get(next) {
                            _ = set(&mut self.draft, pick);
                        }
                    }
                }
            }
            _ => {}
        }

        FormOutcome::Consumed
    }

    /// Handles the mouse over the form: a click focuses a row, opens a text field with the
    /// cursor where it landed (a double-click selects the word there), picks a select option or a
    /// completion candidate, or finishes on the finish row; a drag selects text and the wheel
    /// moves between fields.
    ///
    /// `None` when the mouse is outside the form, so the owner can act on it.
    pub fn handle_mouse(&mut self, mouse: &MouseEvent) -> Option<FormOutcome> {
        let at = Position::new(mouse.column, mouse.row);
        if !self.hits.area.contains(at) {
            return None;
        }
        match mouse.kind {
            MouseEventKind::ScrollUp if self.editor.is_none() => {
                self.cursor = self.cursor.saturating_sub(1);
            }
            MouseEventKind::ScrollDown if self.editor.is_none() => {
                self.cursor = (self.cursor + 1).min(self.visible().len());
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(char) = self.char_at(at, self.cursor)
                    && let Some(editor) = &mut self.editor
                {
                    editor.text.drag_to(char);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => return Some(self.click(at)),
            _ => {}
        }
        Some(FormOutcome::Consumed)
    }

    /// The character of row `row`'s value under `at`, when `at` is on that row's value.
    fn char_at(&self, at: Position, row: usize) -> Option<usize> {
        let (_, _, skip) = self
            .hits
            .rows
            .iter()
            .find(|(y, drawn, _)| *y == at.y && *drawn == row)?;
        let column = usize::from(at.x.checked_sub(self.hits.value_x)?);
        // A scrolled value starts with a `…` standing in for what is cut.
        Some((skip + column).saturating_sub(usize::from(*skip > 0)))
    }

    fn click(&mut self, at: Position) -> FormOutcome {
        if let Some(candidate) = self
            .hits
            .candidates
            .iter()
            .find(|(area, _)| area.contains(at))
            .map(|(_, candidate)| candidate.clone())
        {
            self.use_candidate(&candidate);
            return FormOutcome::Consumed;
        }

        if let Some((field, option)) = self
            .hits
            .options
            .iter()
            .find(|(area, ..)| area.contains(at))
            .map(|(_, field, option)| (*field, *option))
        {
            self.apply_edit();
            self.cursor = field;
            if let Some(WidgetKind::Select { set, .. }) =
                self.visible().get(field).map(|def| &def.widget)
            {
                set(&mut self.draft, option);
            }
            return FormOutcome::Consumed;
        }

        let Some(row) = self
            .hits
            .rows
            .iter()
            .find(|(y, ..)| *y == at.y)
            .map(|(_, row, _)| *row)
        else {
            return FormOutcome::Consumed;
        };
        let double = self
            .last_click
            .is_some_and(|(when, last)| last == row && when.elapsed() < DOUBLE_CLICK);
        self.last_click = Some((Instant::now(), row));

        let visible = self.visible();
        if row != self.cursor {
            self.apply_edit();
            self.cursor = row;
        }
        if row == visible.len() {
            return self.finish();
        }
        let Some(WidgetKind::Text { get, .. }) = visible.get(row).map(|def| &def.widget) else {
            return FormOutcome::Consumed;
        };
        let char = self.char_at(at, row);
        let editor = self
            .editor
            .get_or_insert_with(|| Editor::new(&get(&self.draft)));
        // A click on the label opens the field with the cursor at the end.
        match (char, double) {
            (Some(char), false) => editor.text.click(char),
            (Some(char), true) => editor.text.select_word_at(char),
            (None, _) => {}
        }
        FormOutcome::Consumed
    }

    /// Puts a clicked completion candidate in: in place of the token being completed for a path,
    /// as the whole value for a suggestion.
    fn use_candidate(&mut self, candidate: &str) {
        let (_, is_path) = self.candidates();
        if let Some(editor) = &mut self.editor {
            let start = if is_path { editor.last_token().0 } else { 0 };
            editor.suggestion = 0;
            editor.replace_from(start, candidate);
            return;
        }
        if let Some(def) = self.visible().get(self.cursor)
            && let WidgetKind::Text { set, .. } = &def.widget
        {
            self.error = set(&mut self.draft, candidate).err();
        }
    }

    /// The keys for what is focused, for the legend under the form and the key bar.
    pub fn hints(&self) -> Vec<Hint> {
        if self.editor.is_some() {
            return vec![
                Hint::new("enter", "apply"),
                Hint::new("tab", "complete"),
                Hint::new("ctrl+a", "select all"),
                Hint::new("esc", "cancel"),
            ];
        }
        let mut hints = vec![Hint::new("\u{2191}/\u{2193}", "field")];
        match self.visible().get(self.cursor) {
            Some(def) => match &def.widget {
                WidgetKind::Text { .. } => {
                    hints.push(Hint::new("enter", "edit"));
                    if def
                        .suggest
                        .is_some_and(|suggest| !suggest(&self.draft).is_empty())
                    {
                        hints.push(Hint::new("\u{2190}/\u{2192}", "suggestions"));
                    }
                }
                WidgetKind::Select { .. } => hints.push(Hint::new("space", "choose")),
            },
            None => hints.push(Hint::owned(
                "enter".to_owned(),
                self.finish_label.to_lowercase(),
            )),
        }
        hints.push(Hint::new("esc", "cancel"));
        hints
    }

    /// Completion candidates for the field being edited: filesystem
    /// matches when the current token is a path, the field's suggestions
    /// otherwise. The flag says they are paths (displayed by last
    /// component).
    fn candidates(&self) -> (Vec<String>, bool) {
        // While editing, the current token drives filesystem completion.
        if let Some(editor) = &self.editor {
            let (_, token) = editor.last_token();
            if let Some(target) = completion_target(token) {
                let matches = suggest::complete_path(&target);
                if !matches.is_empty() {
                    return (matches, true);
                }
                if is_path_like(token) {
                    return (Vec::new(), false);
                }
            }
        }

        // Otherwise (and as the fallback) the focused field's suggestions
        // show - also outside edit mode, previewing what ←/→ cycles.
        let suggestions = self
            .visible()
            .get(self.cursor)
            .and_then(|def| def.suggest)
            .map(|suggest| suggest(&self.draft))
            .unwrap_or_default();
        (suggestions, false)
    }

    /// Lines the candidate row needs at `width`.
    fn candidates_height(&self, width: u16) -> u16 {
        let (candidates, is_path) = self.candidates();
        if candidates.is_empty() {
            return 0;
        }
        let total: usize = usize::from(CANDIDATES_PREFIX)
            + candidates
                .iter()
                .take(MAX_SHOWN_CANDIDATES)
                .map(|candidate| shown_candidate(candidate, is_path).chars().count() + 2)
                .sum::<usize>();
        (total as u16).div_ceil(width.max(1)).clamp(1, 3)
    }

    /// Lines the error needs at `width`, capped so a huge message cannot
    /// swallow the form.
    fn error_height(&self, width: u16) -> u16 {
        self.error
            .as_ref()
            .map(|error| {
                let width = (width as usize).max(1);
                ((error.chars().count() + 2).div_ceil(width) as u16).clamp(1, 3)
            })
            .unwrap_or(0)
    }

    /// Lines under the fields: candidates, the error, the focused field's help and the key
    /// legend.
    fn footer_height(&self, width: u16) -> u16 {
        2 + self.error_height(width) + self.candidates_height(width)
    }

    /// Total height a floating dialog needs to show the whole form: fields,
    /// finish row, footer, and borders. Not needed when the form fills a
    /// pane - it scrolls there.
    pub fn dialog_height(&self, width: u16) -> u16 {
        let fields_and_finish = self.visible().len() as u16 + 2;
        fields_and_finish + self.footer_height(width) + 2
    }

    /// Renders the form filling `area`. The field list scrolls when the
    /// registry outgrows the space, so the form works both as a floating
    /// dialog and as a full pane with many settings.
    pub fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let visible = self.visible();
        self.cursor = self.cursor.min(visible.len());

        let inner = dialog(frame, area, self.title);
        let [fields_area, footer_area] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(self.footer_height(inner.width)),
        ])
        .areas(inner);

        let value_x = inner.x + LABEL_WIDTH;
        let value_width = usize::from(inner.width.saturating_sub(LABEL_WIDTH)).max(8);
        let mut lines: Vec<Line> = Vec::new();
        // Per field line: how much of its value is scrolled out, and its options' columns.
        let mut skips = Vec::new();
        let mut options = Vec::new();

        for (index, def) in visible.iter().enumerate() {
            let focused = index == self.cursor;
            let marker = if focused { "› " } else { "  " };
            let label_style = if focused {
                Style::default().fg(theme::INDIGO).bold()
            } else {
                Style::default().fg(theme::GREY)
            };

            let (value_spans, skip) = match (&def.widget, focused, &self.editor) {
                (WidgetKind::Text { .. }, true, Some(editor)) => {
                    let skip = window_start(editor.text.cursor(), value_width);
                    let spans = editor.text.spans(
                        Style::default().fg(theme::LAVENDER),
                        Style::default().fg(theme::INDIGO),
                        true,
                    );
                    (scrolled(spans, skip), skip)
                }
                (WidgetKind::Text { get, .. }, ..) => {
                    let value = get(&self.draft);
                    if value.is_empty() {
                        let unset =
                            Span::styled("(unset)", Style::default().fg(theme::GREY).italic());
                        (vec![unset], 0)
                    } else {
                        // The window ends on the value's tail: for paths it is the identifying
                        // part, and it is where the cursor lands when the field opens.
                        let skip = window_start(value.chars().count(), value_width);
                        let span = Span::styled(value, Style::default().fg(theme::LAVENDER));
                        (scrolled(vec![span], skip), skip)
                    }
                }
                (
                    WidgetKind::Select {
                        options: names,
                        get,
                        ..
                    },
                    ..,
                ) => {
                    let selected = get(&self.draft);
                    let mut spans = Vec::new();
                    let mut x = value_x;
                    for (option, name) in names.iter().enumerate() {
                        let picked = option == selected;
                        let mark = pick_mark(Picks::One, picked);
                        let style = match (picked, focused) {
                            (true, _) => Style::default()
                                .fg(theme::MINT)
                                .add_modifier(Modifier::BOLD),
                            (false, true) => Style::default().fg(theme::LAVENDER),
                            (false, false) => Style::default().fg(theme::GREY),
                        };
                        let width = (mark.width() + name.chars().count()) as u16;
                        options.push((lines.len(), x, width, index, option));
                        x += width + 2;
                        spans.extend([mark, Span::styled(*name, style), Span::raw("  ")]);
                    }
                    (spans, 0)
                }
            };
            skips.push(skip);

            lines.push(Line::from_iter(
                [
                    Span::styled(marker, Style::default().fg(theme::INDIGO)),
                    Span::styled(format!("{:<14}", def.label), label_style),
                ]
                .into_iter()
                .chain(value_spans),
            ));
        }

        lines.push(Line::raw(""));
        let finish_focused = self.cursor == visible.len();
        lines.push(Line::styled(
            format!("  [ {} ]", self.finish_label),
            if finish_focused {
                Style::default().fg(theme::INDIGO).bold().reversed()
            } else {
                Style::default().fg(theme::FILL_DIM)
            },
        ));

        // Scroll just enough to keep the focused row (or the finish row at
        // the very end) in view.
        let finish_line = lines.len() - 1;
        let cursor_line = if finish_focused {
            finish_line
        } else {
            self.cursor
        };
        let top = cursor_line.saturating_sub((fields_area.height as usize).saturating_sub(1));
        frame.render_widget(Paragraph::new(lines).scroll((top as u16, 0)), fields_area);

        let mut hits = Hits {
            area,
            value_x,
            ..Hits::default()
        };
        let line_y = |line: usize| {
            line.checked_sub(top)
                .and_then(|row| u16::try_from(row).ok())
                .filter(|row| *row < fields_area.height)
                .map(|row| fields_area.y + row)
        };
        for (line, skip) in skips.iter().enumerate() {
            if let Some(y) = line_y(line) {
                hits.rows.push((y, line, *skip));
            }
        }
        if let Some(y) = line_y(finish_line) {
            hits.rows.push((y, visible.len(), 0));
        }
        for (line, x, width, field, option) in options {
            if let Some(y) = line_y(line) {
                hits.options
                    .push((Rect::new(x, y, width, 1), field, option));
            }
        }

        let mut footer: Vec<Line> = Vec::new();
        let (candidates, is_path) = self.candidates();
        if !candidates.is_empty() {
            // What lights up in the row: the token being completed, or
            // the field's whole value when cycling outside edit mode.
            let token = match &self.editor {
                Some(editor) => editor.last_token().1.to_owned(),
                None => visible
                    .get(self.cursor)
                    .and_then(|def| match &def.widget {
                        WidgetKind::Text { get, .. } => Some(get(&self.draft)),
                        WidgetKind::Select { .. } => None,
                    })
                    .unwrap_or_default(),
            };
            let mut spans = vec![Span::styled("  ↹ ", Style::default().fg(theme::GREY))];
            let mut x = footer_area.x + CANDIDATES_PREFIX;
            let right = footer_area.x + footer_area.width;
            for candidate in candidates.iter().take(MAX_SHOWN_CANDIDATES) {
                let shown = shown_candidate(candidate, is_path);
                let width = shown.chars().count() as u16;
                // Only the candidates on the row's first line are clicked by position; the ones
                // wrapped below it are still reachable with Tab.
                if x + width <= right {
                    hits.candidates
                        .push((Rect::new(x, footer_area.y, width, 1), candidate.clone()));
                }
                x = x.saturating_add(width + 2);
                // The candidate Tab just picked lights up.
                let style = if *candidate == token {
                    Style::default().fg(theme::INDIGO).bold()
                } else {
                    Style::default().fg(theme::LAVENDER)
                };
                spans.push(Span::styled(shown, style));
                spans.push(Span::raw("  "));
            }
            if candidates.len() > MAX_SHOWN_CANDIDATES {
                spans.push(Span::styled(
                    format!("(+{} more)", candidates.len() - MAX_SHOWN_CANDIDATES),
                    Style::default().fg(theme::GREY).italic(),
                ));
            }
            footer.push(Line::from_iter(spans));
        }
        if let Some(error) = &self.error {
            footer.push(Line::styled(
                format!("  {error}"),
                Style::default().fg(theme::AMBER),
            ));
        }
        let help = visible
            .get(self.cursor)
            .map_or("write the plan and continue", |def| def.help);
        footer.push(Line::styled(
            format!("  {help}"),
            Style::default().fg(theme::GREY).italic(),
        ));
        let mut legend = hints::legend(&self.hints());
        legend.spans.insert(0, Span::raw("  "));
        footer.push(legend);
        frame.render_widget(
            Paragraph::new(footer).wrap(Wrap { trim: false }),
            footer_area,
        );
        self.hits = hits;
    }
}

/// The first character drawn of a value shown `width` cells wide with the cursor at `cursor`:
/// enough is cut from the front to keep the cursor, and the cell after it where typing goes, in
/// view.
fn window_start(cursor: usize, width: usize) -> usize {
    (cursor + 2).saturating_sub(width)
}

/// `spans` with their first `skip` characters cut, behind a `…` when anything was cut.
fn scrolled(spans: Vec<Span<'static>>, skip: usize) -> Vec<Span<'static>> {
    if skip == 0 {
        return spans;
    }
    let mut left = skip;
    let mut kept = vec![Span::styled("…", Style::default().fg(theme::GREY))];
    for span in spans {
        let count = span.content.chars().count();
        if left >= count {
            left -= count;
            continue;
        }
        let content: String = span.content.chars().skip(left).collect();
        left = 0;
        kept.push(Span::styled(content, span.style));
    }
    kept
}

/// Whether a token is a filesystem path to complete rather than a word.
fn is_path_like(token: &str) -> bool {
    token.starts_with('/')
        || token.starts_with("./")
        || token.starts_with("../")
        || token == "~"
        || token.starts_with("~/")
}

/// What the token completes against: itself when it already is a path,
/// the working directory (`./token`) for bare words - the TUI's cwd is
/// where the command runs, so its entries are the natural starting point.
/// Empty tokens complete nothing (the field's suggestions take over).
fn completion_target(token: &str) -> Option<String> {
    if is_path_like(token) {
        Some(token.to_owned())
    } else if token.is_empty() {
        None
    } else {
        Some(format!("./{token}"))
    }
}

/// How a completion candidate shows in the footer row: paths by their last component.
fn shown_candidate(candidate: &str, is_path: bool) -> String {
    if is_path {
        short_name(candidate)
    } else {
        candidate.to_owned()
    }
}

/// The last path component (directories keep their trailing `/`), for
/// showing completion candidates compactly.
fn short_name(candidate: &str) -> String {
    match candidate.trim_end_matches('/').rsplit_once('/') {
        Some((_, name)) if !name.is_empty() => {
            let trailer = if candidate.ends_with('/') { "/" } else { "" };
            format!("{name}{trailer}")
        }
        _ => candidate.to_owned(),
    }
}

/// The service settings registry: one entry per field of the service dialog.
pub const SERVICE_SETTINGS: &[SettingDef<ServiceEntry>] = &[
    SettingDef {
        label: "Name",
        help: "service name, becomes the key in mirrord-up.yaml",
        visible: |_| true,
        suggest: None,
        widget: WidgetKind::Text {
            get: |entry| entry.name.clone(),
            set: |entry, value| {
                if value.is_empty() {
                    return Err("the service needs a name".to_owned());
                }
                entry.name = value.to_owned();
                Ok(())
            },
        },
    },
    SettingDef {
        label: "Mode",
        help: "split shares traffic via an HTTP filter; replace takes it all over",
        visible: |_| true,
        suggest: None,
        widget: WidgetKind::Select {
            // Must track `ServiceMode`'s strum serialization; a unit test
            // below keeps them in sync.
            options: &["split", "replace"],
            get: |entry| {
                ServiceMode::VARIANTS
                    .iter()
                    .position(|mode| *mode == entry.spec.default_mode)
                    .unwrap_or_default()
            },
            set: |entry, index| {
                // Taken modulo `VARIANTS`' own length.
                #[allow(clippy::indexing_slicing)]
                let mode = ServiceMode::VARIANTS[index % ServiceMode::VARIANTS.len()];
                entry.spec.default_mode = mode;
            },
        },
    },
    SettingDef {
        label: "HTTP filter",
        help: "header filter like `x-user: me`; empty = auto session filter",
        visible: |entry| entry.spec.default_mode == ServiceMode::Split,
        suggest: None,
        widget: WidgetKind::Text {
            get: |entry| {
                entry
                    .spec
                    .http_filter
                    .as_ref()
                    .and_then(|filter| filter.header_filter.clone())
                    .unwrap_or_default()
            },
            set: |entry, value| {
                entry.spec.http_filter = if value.is_empty() {
                    None
                } else {
                    Some(HttpFilterSpec {
                        header_filter: Some(value.to_owned()),
                    })
                };
                Ok(())
            },
        },
    },
    SettingDef {
        label: "Ignore ports",
        help: "local ports mirrord leaves alone, comma-separated (e.g. 9229)",
        visible: |_| true,
        suggest: None,
        widget: WidgetKind::Text {
            get: |entry| {
                entry
                    .spec
                    .ignore_ports
                    .iter()
                    .map(u16::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            },
            set: |entry, value| {
                entry.spec.ignore_ports = value
                    .split(',')
                    .map(str::trim)
                    .filter(|part| !part.is_empty())
                    .map(|part| {
                        part.parse::<u16>()
                            .map_err(|_| format!("`{part}` is not a port number"))
                    })
                    .collect::<Result<_, _>>()?;
                Ok(())
            },
        },
    },
    SettingDef {
        label: "Directory",
        help: "where the command runs - defaults to where the TUI was started",
        visible: |_| true,
        suggest: None,
        widget: WidgetKind::Text {
            get: |entry| {
                entry
                    .spec
                    .run
                    .dir
                    .as_deref()
                    .map(suggest::compress_home)
                    .unwrap_or_default()
            },
            set: |entry, value| {
                // Completion leaves a trailing `/` on directories; strip
                // it (but never turn the root path into nothing).
                let value = if value.len() > 1 {
                    value.trim_end_matches('/')
                } else {
                    value
                };
                entry.spec.run.dir = (!value.is_empty()).then(|| suggest::expand_home(value));
                Ok(())
            },
        },
    },
    SettingDef {
        label: "Command",
        help: "how you start this service locally (e.g. `npm start`) - needed only to run",
        visible: |_| true,
        suggest: Some(|entry| {
            // What ran for this target before comes first; then whatever
            // the directory's project markers suggest.
            let mut suggestions = history::recall(&history::target_key(&entry.spec.target))
                .map(|past| past.commands)
                .unwrap_or_default();
            for command in suggest::commands_in(entry.spec.run.dir.as_deref()) {
                if !suggestions.contains(&command) {
                    suggestions.push(command);
                }
            }
            suggestions
        }),
        widget: WidgetKind::Text {
            get: |entry| {
                let parts: Vec<String> = entry
                    .spec
                    .run
                    .command
                    .iter()
                    .map(|part| suggest::compress_home(part))
                    .collect();
                join_command(&parts)
            },
            set: |entry, value| {
                // `~` works here even though no shell is involved: paths
                // are stored expanded and displayed compressed.
                entry.spec.run.command = split_command(value)?
                    .into_iter()
                    .map(|part| suggest::expand_home(&part))
                    .collect();
                Ok(())
            },
        },
    },
];

/// Validation run when saving a service. The command is deliberately not
/// required here - picking targets and filters is the main flow, and the
/// run command only matters once the user actually runs the plan.
pub fn validate_service(entry: &ServiceEntry) -> Result<(), String> {
    if entry.name.trim().is_empty() {
        return Err("the service needs a name".to_owned());
    }
    if let Some(dir) = &entry.spec.run.dir
        && !std::path::Path::new(dir).is_dir()
    {
        return Err(format!(
            "directory `{}` does not exist",
            suggest::compress_home(dir)
        ));
    }
    Ok(())
}

/// Seeds a service draft from a picked target.
pub fn draft_for_target(path: &str, namespace: &str, workload_name: &str) -> ServiceEntry {
    let target = if path == "targetless" {
        TargetSpec::Targetless
    } else {
        TargetSpec::Path {
            path: path.to_owned(),
            namespace: Some(namespace.to_owned()),
        }
    };

    // A target that ran before prefills with its own history - the user's
    // prior choice, not a guess. Otherwise the directory prefills with
    // the TUI's cwd (a factual default) and the command stays empty on
    // purpose: prefilling a *detected* command reads as authoritative and
    // gets saved unnoticed (running `cargo run` from this repo would
    // launch the TUI itself as the service).
    let past = history::recall(&history::target_key(&target));
    let dir = past.as_ref().and_then(|past| past.dir.clone()).or_else(|| {
        std::env::current_dir()
            .ok()
            .map(|dir| dir.display().to_string())
    });
    let command = past
        .as_ref()
        .and_then(|past| past.commands.first())
        .map(|command| split_command(command).unwrap_or_default())
        .unwrap_or_default();

    ServiceEntry {
        name: workload_name.to_owned(),
        spec: ServiceSpec {
            target,
            default_mode: ServiceMode::Split,
            http_filter: None,
            ignore_ports: Default::default(),
            skip: false,
            config_patch: None,
            run: RunSpec {
                dir,
                command,
                ..Default::default()
            },
        },
    }
}

/// Splits a command line on whitespace, honoring single and double quotes,
/// so `docker run -e MSG="hello world" app` keeps the message together.
pub fn split_command(line: &str) -> Result<Vec<String>, String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_part = false;
    let mut quote: Option<char> = None;

    for c in line.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => current.push(c),
            None if c == '\'' || c == '"' => {
                quote = Some(c);
                in_part = true;
            }
            None if c.is_whitespace() => {
                if in_part {
                    parts.push(std::mem::take(&mut current));
                    in_part = false;
                }
            }
            None => {
                current.push(c);
                in_part = true;
            }
        }
    }

    if quote.is_some() {
        return Err("unclosed quote in the command".to_owned());
    }
    if in_part {
        parts.push(current);
    }
    Ok(parts)
}

/// Inverse of [`split_command`] for displaying the stored command.
pub fn join_command(parts: &[String]) -> String {
    parts
        .iter()
        .map(|part| {
            if part.is_empty() || part.chars().any(char::is_whitespace) {
                format!("\"{part}\"")
            } else {
                part.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use crossterm::event::{KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend};

    use super::*;

    fn service_form() -> SettingsForm<ServiceEntry> {
        let entry = draft_for_target("pod/app", "ns", "app");
        SettingsForm::new(entry, SERVICE_SETTINGS, " t ", "ok", validate_service)
    }

    fn press(form: &mut SettingsForm<ServiceEntry>, code: KeyCode, modifiers: KeyModifiers) {
        form.handle_key(KeyEvent::new(code, modifiers));
    }

    fn draw(form: &mut SettingsForm<ServiceEntry>) {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| form.draw(frame, frame.area()))
            .unwrap();
    }

    fn mouse(form: &mut SettingsForm<ServiceEntry>, kind: MouseEventKind, column: u16, row: u16) {
        form.handle_mouse(&MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        });
    }

    /// The line the field labelled `label` was drawn on.
    fn row_of(form: &SettingsForm<ServiceEntry>, label: &str) -> u16 {
        let index = form
            .visible()
            .iter()
            .position(|def| def.label == label)
            .unwrap();
        form.hits
            .rows
            .iter()
            .find(|(_, row, _)| *row == index)
            .map(|(y, ..)| *y)
            .unwrap()
    }

    /// On a focused (not edited) Command row, ←/→ cycle the suggestions
    /// straight into the value, like a select.
    /// Unix-only: the fixture it cycles over is made runnable with a mode bit, which is the very
    /// thing other platforms do not have.
    #[cfg(unix)]
    #[test]
    fn arrows_cycle_suggestions_on_a_focused_text_field() {
        let dir =
            std::env::temp_dir().join(format!("mirrord-tui-form-cycle-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let binary = dir.join("app");
        std::fs::write(&binary, "").unwrap();
        let mut perms = std::fs::metadata(&binary).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&binary, perms).unwrap();

        let mut entry = draft_for_target("pod/app", "ns", "app");
        entry.spec.run.dir = Some(dir.display().to_string());
        let mut form = SettingsForm::new(entry, SERVICE_SETTINGS, " t ", "ok", validate_service);

        // Walk down to the Command row: Name, Mode, HTTP filter,
        // Ignore ports, Directory, then Command.
        for _ in 0..5 {
            press(&mut form, KeyCode::Down, KeyModifiers::NONE);
        }
        press(&mut form, KeyCode::Right, KeyModifiers::NONE);

        assert_eq!(form.draft.spec.run.command, ["./app"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Tab on a field that has a suggestion list but nothing in it does nothing, rather than
    /// cycling through an empty list.
    #[test]
    fn tab_with_no_suggestions_leaves_the_value_alone() {
        let mut editor = Editor::new("npm start");
        editor.cycle_suggestion(&[]);
        assert_eq!(editor.text.value(), "npm start");
    }

    /// Bare words complete against the working directory - where the
    /// command will actually run - while explicit paths stay themselves.
    #[test]
    fn completion_targets_bare_words_at_the_working_directory() {
        assert_eq!(completion_target("zo"), Some("./zo".to_owned()));
        assert_eq!(completion_target("/a/b"), Some("/a/b".to_owned()));
        assert_eq!(completion_target("~/a"), Some("~/a".to_owned()));
        assert_eq!(completion_target(""), None);
    }

    /// `Ctrl+A` selects the whole value, as in every other text box of the interface, and
    /// typing replaces it. On a field not being edited yet it opens the editor by itself.
    #[test]
    fn ctrl_a_selects_the_whole_value() {
        let mut form = service_form();
        press(&mut form, KeyCode::Char('a'), KeyModifiers::CONTROL);
        assert!(form.editor.is_some(), "the editor opened by itself");
        press(&mut form, KeyCode::Char('x'), KeyModifiers::NONE);
        press(&mut form, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(form.draft.name, "x", "typing replaced the selection");
    }

    /// A selection key on a focused (not yet edited) text field opens
    /// the editor and applies immediately - no Enter needed first.
    #[test]
    fn shift_selection_opens_the_editor_from_the_focused_row() {
        let mut form = service_form();

        // The Name field is focused; shift+Home selects its whole value.
        press(&mut form, KeyCode::Home, KeyModifiers::SHIFT);
        assert!(form.editor.is_some(), "the editor opened by itself");
        press(&mut form, KeyCode::Char('x'), KeyModifiers::NONE);
        press(&mut form, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(form.draft.name, "x", "typing replaced the selection");
    }

    /// The edit window follows the cursor: it stays visible with a
    /// leading `…` once the value outgrows the field.
    #[test]
    fn long_values_scroll_to_keep_the_cursor_in_view() {
        let rendered = |spans: Vec<Span<'static>>| -> String {
            spans.iter().map(|span| span.content.as_ref()).collect()
        };
        let mut input = TextInput::default();
        input.paste("abcdefghij");
        let spans = || input.spans(Style::default(), Style::default(), true);

        assert_eq!(
            rendered(scrolled(spans(), window_start(10, 6))),
            "…ghij\u{2588}"
        );
        assert_eq!(window_start(0, 6), 0, "the start is in view as it is");
    }

    /// A click on a text field opens it with the cursor where the click landed, a double-click
    /// selects the word there, and a click on another row applies the edit.
    #[test]
    fn clicks_edit_text_fields() {
        let mut form = service_form();
        form.draft.name = "shop api".to_owned();
        draw(&mut form);

        let name = row_of(&form, "Name");
        let value_x = form.hits.value_x;
        mouse(
            &mut form,
            MouseEventKind::Down(MouseButton::Left),
            value_x + 2,
            name,
        );
        press(&mut form, KeyCode::Char('i'), KeyModifiers::NONE);
        assert_eq!(form.editor.as_ref().unwrap().text.value(), "shiop api");

        draw(&mut form);
        mouse(
            &mut form,
            MouseEventKind::Down(MouseButton::Left),
            value_x + 7,
            name,
        );
        assert!(form.editor.as_ref().unwrap().text.has_selection());
        press(&mut form, KeyCode::Char('x'), KeyModifiers::NONE);

        let directory = row_of(&form, "Directory");
        mouse(
            &mut form,
            MouseEventKind::Down(MouseButton::Left),
            2,
            directory,
        );
        assert_eq!(form.draft.name, "shiop x", "leaving the field applied it");
    }

    /// A drag across a field's value selects what it passes over.
    #[test]
    fn dragging_selects_text() {
        let mut form = service_form();
        form.draft.name = "checkout".to_owned();
        draw(&mut form);

        let name = row_of(&form, "Name");
        let value_x = form.hits.value_x;
        mouse(
            &mut form,
            MouseEventKind::Down(MouseButton::Left),
            value_x,
            name,
        );
        mouse(
            &mut form,
            MouseEventKind::Drag(MouseButton::Left),
            value_x + 5,
            name,
        );
        press(&mut form, KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(form.editor.as_ref().unwrap().text.value(), "xout");
    }

    /// A select field shows every option with the shared pick mark, and a click on one picks
    /// it.
    #[test]
    fn clicking_an_option_picks_it() {
        let mut form = service_form();
        draw(&mut form);

        let (area, ..) = form
            .hits
            .options
            .iter()
            .find(|(_, _, option)| *option == 1)
            .copied()
            .unwrap();
        mouse(
            &mut form,
            MouseEventKind::Down(MouseButton::Left),
            area.x,
            area.y,
        );
        assert_eq!(form.draft.spec.default_mode, ServiceMode::Replace);

        press(&mut form, KeyCode::Char(' '), KeyModifiers::NONE);
        assert_eq!(
            form.draft.spec.default_mode,
            ServiceMode::Split,
            "space picks the next option"
        );
    }

    #[test]
    fn split_command_honors_quotes() {
        assert_eq!(
            split_command("docker run -e MSG=\"hello world\" app").unwrap(),
            ["docker", "run", "-e", "MSG=hello world", "app"],
        );
        assert_eq!(split_command("cargo run").unwrap(), ["cargo", "run"]);
        assert!(split_command("echo \"open").is_err());
    }

    #[test]
    fn join_command_round_trips_spaced_arguments() {
        let parts = vec!["npm".to_owned(), "run".to_owned(), "dev server".to_owned()];
        assert_eq!(split_command(&join_command(&parts)).unwrap(), parts);
    }

    /// The Mode select's option labels are a hand-written list (const
    /// registries cannot call strum at compile time); this pins them to the
    /// enum's actual serialization.
    #[test]
    fn mode_options_match_the_enum_variants() {
        let mode = SERVICE_SETTINGS
            .iter()
            .find(|def| def.label == "Mode")
            .expect("the registry has a Mode entry");
        let WidgetKind::Select { options, .. } = &mode.widget else {
            panic!("Mode is a select");
        };

        let variants: Vec<&'static str> = ServiceMode::VARIANTS
            .iter()
            .map(<&'static str>::from)
            .collect();
        assert_eq!(*options, variants.as_slice());
    }
}
