//! Generic settings form used by the service dialog and the export dialog.
//!
//! Every field is one entry in a registry (`&[SettingDef<T>]`), so adding a
//! setting later - env overrides, ports mapping, anything the config schema
//! grows - is one model field plus one registry entry, no new UI code. The
//! registry is also the seam for generating entries from `mirrord-schema.json`
//! once the wizard goes fully dynamic.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};
use strum::VariantArray;

use crate::{
    helpers::dialog,
    screens::targets::{
        history, keys,
        model::{HttpFilterSpec, RunSpec, ServiceEntry, ServiceMode, ServiceSpec, TargetSpec},
        suggest, theme,
    },
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
    /// One of a fixed set of options, cycled with Enter or arrow keys.
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

/// A single-line text editor for the field being edited.
struct Input {
    value: String,
    cursor: usize,
    /// Selection anchor; a selection spans `anchor..cursor` (either way).
    anchor: Option<usize>,
    /// Next suggestion Tab inserts.
    suggestion: usize,
}

impl Input {
    fn new(value: String) -> Self {
        let cursor = value.chars().count();
        Self {
            value,
            cursor,
            anchor: None,
            suggestion: 0,
        }
    }

    /// The active selection as a normalized char range, if any.
    fn selection(&self) -> Option<(usize, usize)> {
        let anchor = self.anchor?;
        if anchor == self.cursor {
            return None;
        }
        Some((anchor.min(self.cursor), anchor.max(self.cursor)))
    }

    /// Moves the cursor; with `select` the selection extends from where
    /// the cursor was, without it any selection collapses.
    fn move_to(&mut self, target: usize, select: bool) {
        if select {
            self.anchor.get_or_insert(self.cursor);
        } else {
            self.anchor = None;
        }
        self.cursor = target.min(self.value.chars().count());
    }

    /// Deletes the selection if one is active; `true` when it was.
    fn delete_selection(&mut self) -> bool {
        let Some((from, to)) = self.selection() else {
            self.anchor = None;
            return false;
        };
        self.anchor = None;
        self.delete_chars(from, to);
        true
    }

    /// Replaces the value with the next suggestion, wrapping around.
    fn cycle_suggestion(&mut self, suggestions: &[String]) {
        let Some(suggestion) = suggestions.get(self.suggestion % suggestions.len()) else {
            return;
        };
        self.suggestion += 1;
        self.value = suggestion.clone();
        self.cursor = self.value.chars().count();
    }

    /// The token being completed: everything after the last space.
    fn last_token(&self) -> (usize, &str) {
        let start = self.value.rfind(' ').map(|at| at + 1).unwrap_or(0);
        (start, &self.value[start..])
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
        } else if let Some(pick) = candidates.get(self.suggestion % candidates.len()) {
            self.suggestion += 1;
            pick.clone()
        } else {
            return true;
        };

        self.value.truncate(start);
        self.value.push_str(&replacement);
        self.cursor = self.value.chars().count();
        true
    }

    /// The value windowed to `width` so the cursor stays visible however
    /// long the value grows; the cursor renders as a reversed cell and
    /// the selection as a filled background, with a leading `…` when
    /// scrolled.
    fn spans(&self, width: usize) -> Vec<Span<'static>> {
        let chars: Vec<char> = self.value.chars().collect();
        let selection = self.selection();
        // One virtual cell past the end so the cursor can sit there.
        let total = chars.len() + 1;
        let skip = (self.cursor + 2).saturating_sub(width);

        let mut spans = Vec::new();
        if skip > 0 {
            spans.push(Span::styled("…", Style::default().fg(theme::TEXT_MUTED)));
        }
        let take = width.saturating_sub(usize::from(skip > 0));
        for at in skip..total.min(skip + take) {
            let cell = chars.get(at).copied().unwrap_or(' ');
            let mut style = Style::default().fg(theme::TEXT_EMPHASIS).underlined();
            if selection.is_some_and(|(from, to)| at >= from && at < to) {
                style = style.bg(theme::FILL_HEAVY);
            }
            if at == self.cursor {
                style = style.reversed();
            }
            spans.push(Span::styled(cell.to_string(), style));
        }
        spans
    }

    /// Inserts pasted text at the cursor. Control characters collapse to
    /// spaces: fields are single-line, and the trailing newline most
    /// clipboards carry must not end up in the value.
    fn paste(&mut self, text: &str) {
        self.suggestion = 0;
        self.delete_selection();
        let cleaned: String = text
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        let cleaned = cleaned.trim();
        let at = self.byte_cursor();
        self.value.insert_str(at, cleaned);
        self.cursor += cleaned.chars().count();
    }

    fn byte_cursor(&self) -> usize {
        self.byte_at(self.cursor)
    }

    fn byte_at(&self, at: usize) -> usize {
        self.value
            .char_indices()
            .nth(at)
            .map(|(index, _)| index)
            .unwrap_or(self.value.len())
    }

    /// Deletes the character range `from..to` and parks the cursor at the
    /// start of it.
    fn delete_chars(&mut self, from: usize, to: usize) {
        let range = self.byte_at(from)..self.byte_at(to);
        self.value.replace_range(range, "");
        self.cursor = from;
    }

    /// The word boundary left of the cursor: skips spaces, then the word.
    fn word_start(&self) -> usize {
        let chars: Vec<char> = self.value.chars().collect();
        let mut at = self.cursor.min(chars.len());
        while at > 0 && chars.get(at - 1) == Some(&' ') {
            at -= 1;
        }
        while at > 0 && chars.get(at - 1) != Some(&' ') {
            at -= 1;
        }
        at
    }

    /// The word boundary right of the cursor: skips spaces, then to the
    /// end of the word.
    fn word_end(&self) -> usize {
        let chars: Vec<char> = self.value.chars().collect();
        let mut at = self.cursor.min(chars.len());
        while chars.get(at) == Some(&' ') {
            at += 1;
        }
        while chars.get(at).is_some_and(|c| *c != ' ') {
            at += 1;
        }
        at
    }

    fn handle_key(&mut self, key: KeyEvent) {
        // Any edit restarts suggestion and completion cycling.
        self.suggestion = 0;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        // Cmd on macOS; only terminals speaking the kitty keyboard
        // protocol deliver it.
        let cmd = key.modifiers.contains(KeyModifiers::SUPER);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let end = self.value.chars().count();

        match key.code {
            // Mac conventions: ⌘a select all, ⌘←/→ line ends, ⌘⌫ kill
            // to start; ⌥ jumps words. Shift extends the selection.
            // ⌥a doubles as select-all for terminals that never forward
            // ⌘ (it activates the menu bar in e.g. Warp).
            KeyCode::Char('a') if cmd || alt => {
                self.anchor = Some(0);
                self.cursor = end;
            }
            KeyCode::Backspace if cmd => {
                if !self.delete_selection() {
                    self.delete_chars(0, self.cursor);
                }
            }
            // Readline, the terminal-native conventions: ^a/^e line
            // ends, ^u/^k kill to start/end, ^w word back.
            KeyCode::Char('a') if ctrl => self.move_to(0, shift),
            KeyCode::Char('e') if ctrl => self.move_to(end, shift),
            KeyCode::Char('u') if ctrl => self.delete_chars(0, self.cursor),
            KeyCode::Char('k') if ctrl => {
                let at = self.byte_cursor();
                self.value.truncate(at);
            }
            KeyCode::Char('w') if ctrl => self.delete_chars(self.word_start(), self.cursor),
            KeyCode::Backspace if alt => {
                if !self.delete_selection() {
                    self.delete_chars(self.word_start(), self.cursor);
                }
            }
            KeyCode::Left if cmd => self.move_to(0, shift),
            KeyCode::Right if cmd => self.move_to(end, shift),
            KeyCode::Left if ctrl || alt => self.move_to(self.word_start(), shift),
            KeyCode::Right if ctrl || alt => self.move_to(self.word_end(), shift),
            KeyCode::Char(c) if !ctrl && !alt && !cmd => {
                self.delete_selection();
                let at = self.byte_cursor();
                self.value.insert(at, c);
                self.cursor += 1;
            }
            KeyCode::Backspace => {
                if !self.delete_selection() && self.cursor > 0 {
                    self.cursor -= 1;
                    let at = self.byte_cursor();
                    self.value.remove(at);
                }
            }
            KeyCode::Delete => {
                if !self.delete_selection() && self.cursor < end {
                    let at = self.byte_cursor();
                    self.value.remove(at);
                }
            }
            // A plain arrow with a selection collapses to its edge, the
            // mac way; shift+arrow extends it.
            KeyCode::Left => match (self.selection(), shift) {
                (Some((from, _)), false) => self.move_to(from, false),
                _ => self.move_to(self.cursor.saturating_sub(1), shift),
            },
            KeyCode::Right => match (self.selection(), shift) {
                (Some((_, to)), false) => self.move_to(to, false),
                _ => self.move_to(self.cursor + 1, shift),
            },
            KeyCode::Home => self.move_to(0, shift),
            KeyCode::End => self.move_to(end, shift),
            _ => {}
        }
    }
}

/// What a key press in the form resulted in.
pub enum FormOutcome {
    Consumed,
    /// Esc outside of text editing.
    Cancelled,
    /// The finish row was activated and validation passed; the draft is ready.
    Finished,
}

pub struct SettingsForm<T: 'static> {
    pub draft: T,
    settings: &'static [SettingDef<T>],
    title: &'static str,
    finish_label: &'static str,
    validate: fn(&T) -> Result<(), String>,
    /// Index into the *visible* fields; one past the end is the finish row.
    cursor: usize,
    input: Option<Input>,
    error: Option<String>,
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
            input: None,
            error: None,
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
        if let WidgetKind::Text { get, .. } = &visible[index].widget {
            self.input = Some(Input::new(get(&self.draft)));
        }
    }

    fn visible(&self) -> Vec<&'static SettingDef<T>> {
        self.settings
            .iter()
            .filter(|def| (def.visible)(&self.draft))
            .collect()
    }

    /// True while a text field is capturing keystrokes.
    pub fn typing(&self) -> bool {
        self.input.is_some()
    }

    /// Routes a terminal paste into the field being edited; `false` when
    /// no text editor is open to take it.
    pub fn paste(&mut self, text: &str) -> bool {
        let Some(input) = &mut self.input else {
            return false;
        };
        input.paste(text);
        true
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> FormOutcome {
        let visible = self.visible();

        if let Some(input) = &mut self.input {
            match key.code {
                KeyCode::Esc => self.input = None,
                KeyCode::Tab => {
                    // A path-like token completes against the filesystem;
                    // anything else cycles the field's suggestions.
                    if !input.complete_path()
                        && let Some(suggest) = visible.get(self.cursor).and_then(|def| def.suggest)
                    {
                        input.cycle_suggestion(&suggest(&self.draft));
                    }
                }
                KeyCode::Enter => {
                    let value = input.value.clone();
                    self.input = None;
                    if let Some(def) = visible.get(self.cursor)
                        && let WidgetKind::Text { set, .. } = &def.widget
                    {
                        match set(&mut self.draft, value.trim()) {
                            Ok(()) => self.error = None,
                            Err(message) => self.error = Some(message),
                        }
                    }
                }
                _ => input.handle_key(key),
            }
            return FormOutcome::Consumed;
        }

        // Selection keys on a focused text field open its editor and
        // apply immediately, so the field behaves as if always editable
        // instead of demanding an Enter first.
        if let Some(def) = visible.get(self.cursor)
            && let WidgetKind::Text { get, .. } = &def.widget
        {
            let shift_move = key.modifiers.contains(KeyModifiers::SHIFT)
                && matches!(
                    key.code,
                    KeyCode::Left | KeyCode::Right | KeyCode::Home | KeyCode::End
                );
            let select_all = key
                .modifiers
                .intersects(KeyModifiers::SUPER | KeyModifiers::ALT)
                && matches!(key.code, KeyCode::Char('a'));
            if shift_move || select_all {
                let mut input = Input::new(get(&self.draft));
                input.handle_key(key);
                self.input = Some(input);
                return FormOutcome::Consumed;
            }
        }

        match key.code {
            KeyCode::Esc => return FormOutcome::Cancelled,
            KeyCode::Up | KeyCode::Char(keys::UP) => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char(keys::DOWN) => {
                self.cursor = (self.cursor + 1).min(visible.len());
            }
            KeyCode::Enter => {
                if self.cursor == visible.len() {
                    match (self.validate)(&self.draft) {
                        Ok(()) => return FormOutcome::Finished,
                        Err(message) => self.error = Some(message),
                    }
                } else if let Some(def) = visible.get(self.cursor) {
                    match &def.widget {
                        WidgetKind::Text { get, .. } => {
                            self.input = Some(Input::new(get(&self.draft)));
                        }
                        WidgetKind::Select { options, get, set } => {
                            let next = (get(&self.draft) + 1) % options.len();
                            set(&mut self.draft, next);
                        }
                    }
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

    /// Completion candidates for the field being edited: filesystem
    /// matches when the current token is a path, the field's suggestions
    /// otherwise. The flag says they are paths (displayed by last
    /// component).
    fn candidates(&self) -> (Vec<String>, bool) {
        // While editing, the current token drives filesystem completion.
        if let Some(input) = &self.input {
            let (_, token) = input.last_token();
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
        let total: usize = 4 + candidates
            .iter()
            .take(MAX_SHOWN_CANDIDATES)
            .map(|candidate| {
                let shown = if is_path {
                    short_name(candidate)
                } else {
                    candidate.clone()
                };
                shown.chars().count() + 2
            })
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

    /// Total height a floating dialog needs to show the whole form: fields,
    /// finish row, footer, and borders. Not needed when the form fills a
    /// pane - it scrolls there.
    pub fn dialog_height(&self, width: u16) -> u16 {
        let fields_and_finish = self.visible().len() as u16 + 2;
        let footer = 1 + self.error_height(width) + self.candidates_height(width);
        fields_and_finish + footer + 2
    }

    /// Renders the form filling `area`. The field list scrolls when the
    /// registry outgrows the space, so the form works both as a floating
    /// dialog and as a full pane with many settings.
    pub fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let visible = self.visible();
        self.cursor = self.cursor.min(visible.len());

        let inner = dialog(frame, area, self.title);
        let footer_height =
            1 + self.error_height(inner.width) + self.candidates_height(inner.width);
        let [fields_area, footer_area] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(footer_height)]).areas(inner);

        let mut lines: Vec<Line> = Vec::new();

        // The value column: whatever the marker and label leave over.
        let value_width = (inner.width as usize).saturating_sub(16).max(8);

        for (index, def) in visible.iter().enumerate() {
            let focused = index == self.cursor;
            let marker = if focused { "› " } else { "  " };
            let label_style = if focused {
                Style::default().fg(theme::BRAND).bold()
            } else {
                Style::default().fg(theme::TEXT_MUTED)
            };

            let value_spans = match (&def.widget, focused, &self.input) {
                (WidgetKind::Text { .. }, true, Some(input)) => input.spans(value_width),
                (WidgetKind::Text { get, .. }, ..) => {
                    let value = get(&self.draft);
                    let span = if value.is_empty() {
                        Span::styled("(unset)", Style::default().fg(theme::TEXT_MUTED).italic())
                    } else {
                        // Keep the tail: for paths it is the identifying part.
                        Span::styled(
                            tail_ellipsize(&value, value_width),
                            Style::default().fg(theme::TEXT_EMPHASIS),
                        )
                    };
                    vec![span]
                }
                (WidgetKind::Select { options, get, .. }, ..) => {
                    let selected = get(&self.draft);
                    vec![return_spans_select(options, selected, focused)]
                }
            };

            lines.push(Line::from_iter(
                [
                    Span::styled(marker, Style::default().fg(theme::BRAND)),
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
                Style::default().fg(theme::BRAND).bold().reversed()
            } else {
                Style::default().fg(theme::FILL_DIM)
            },
        ));

        // Scroll just enough to keep the focused row (or the finish row at
        // the very end) in view.
        let cursor_line = if finish_focused {
            lines.len() - 1
        } else {
            self.cursor
        };
        let top = cursor_line.saturating_sub((fields_area.height as usize).saturating_sub(1));
        frame.render_widget(Paragraph::new(lines).scroll((top as u16, 0)), fields_area);

        let mut footer: Vec<Line> = Vec::new();
        let (candidates, is_path) = self.candidates();
        if !candidates.is_empty() {
            // What lights up in the row: the token being completed, or
            // the field's whole value when cycling outside edit mode.
            let token = match &self.input {
                Some(input) => input.last_token().1.to_owned(),
                None => self
                    .visible()
                    .get(self.cursor)
                    .and_then(|def| match &def.widget {
                        WidgetKind::Text { get, .. } => Some(get(&self.draft)),
                        WidgetKind::Select { .. } => None,
                    })
                    .unwrap_or_default(),
            };
            let mut spans = vec![Span::styled("  ↹ ", Style::default().fg(theme::TEXT_MUTED))];
            for candidate in candidates.iter().take(MAX_SHOWN_CANDIDATES) {
                let shown = if is_path {
                    short_name(candidate)
                } else {
                    candidate.clone()
                };
                // The candidate Tab just picked lights up.
                let style = if *candidate == token {
                    Style::default().fg(theme::BRAND).bold()
                } else {
                    Style::default().fg(theme::TEXT_EMPHASIS)
                };
                spans.push(Span::styled(shown, style));
                spans.push(Span::raw("  "));
            }
            if candidates.len() > MAX_SHOWN_CANDIDATES {
                spans.push(Span::styled(
                    format!("(+{} more)", candidates.len() - MAX_SHOWN_CANDIDATES),
                    Style::default().fg(theme::TEXT_MUTED).italic(),
                ));
            }
            footer.push(Line::from_iter(spans));
        }
        if let Some(error) = &self.error {
            footer.push(Line::styled(
                format!("  {error}"),
                Style::default().fg(theme::WARNING),
            ));
        }
        let help = match visible.get(self.cursor) {
            Some(def) if self.input.is_some() => {
                let tab_hint = if def
                    .suggest
                    .is_some_and(|suggest| !suggest(&self.draft).is_empty())
                {
                    " · Tab completes paths & suggestions"
                } else {
                    " · Tab completes paths"
                };
                format!("{}{tab_hint}", def.help)
            }
            Some(def)
                if def
                    .suggest
                    .is_some_and(|suggest| !suggest(&self.draft).is_empty()) =>
            {
                format!("{} · ←/→ cycle suggestions", def.help)
            }
            Some(def) => def.help.to_owned(),
            None => "write the plan and continue".to_owned(),
        };
        footer.push(Line::styled(
            format!("  {help}"),
            Style::default().fg(theme::TEXT_MUTED).italic(),
        ));
        frame.render_widget(
            Paragraph::new(footer).wrap(Wrap { trim: false }),
            footer_area,
        );
    }
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

/// Clamps `text` to its LAST `max` characters with a leading `…`; the
/// tail is what identifies long values like filesystem paths.
fn tail_ellipsize(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_owned();
    }
    std::iter::once('…')
        .chain(text.chars().skip(count + 1 - max))
        .collect()
}

/// Renders a select value as `option-a | option-b` with the active one lit.
fn return_spans_select(options: &[&'static str], selected: usize, focused: bool) -> Span<'static> {
    let rendered = options
        .iter()
        .enumerate()
        .map(|(index, option)| {
            if index == selected {
                format!("[{option}]")
            } else {
                (*option).to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    Span::styled(
        rendered,
        if focused {
            Style::default().fg(theme::TEXT_EMPHASIS)
        } else {
            Style::default().fg(theme::TEXT_MUTED)
        },
    )
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
                entry.spec.default_mode =
                    ServiceMode::VARIANTS[index % ServiceMode::VARIANTS.len()];
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
    use super::*;

    /// On a focused (not edited) Command row, ←/→ cycle the suggestions
    /// straight into the value, like a select.
    #[test]
    fn arrows_cycle_suggestions_on_a_focused_text_field() {
        use std::os::unix::fs::PermissionsExt;

        use crossterm::event::{KeyEvent, KeyModifiers};

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
            form.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        form.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));

        assert_eq!(form.draft.spec.run.command, ["./app"]);
        std::fs::remove_dir_all(dir).unwrap();
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

    /// The single-line editor speaks readline: ^a/^e/^u/^k/^w and
    /// word-wise movement.
    #[test]
    fn readline_keys_edit_the_line() {
        use crossterm::event::KeyModifiers;

        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        let mut input = Input::new("cargo run --release".to_owned());

        input.handle_key(ctrl('w'));
        assert_eq!(input.value, "cargo run ");
        input.handle_key(ctrl('a'));
        assert_eq!(input.cursor, 0);
        input.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::ALT));
        assert_eq!(input.cursor, 5, "alt+right lands at the word's end");
        input.handle_key(ctrl('k'));
        assert_eq!(input.value, "cargo");
        input.handle_key(ctrl('u'));
        assert_eq!(input.value, "");
    }

    #[test]
    fn tail_ellipsize_keeps_the_identifying_tail() {
        assert_eq!(tail_ellipsize("short", 10), "short");
        assert_eq!(tail_ellipsize("/a/long/path/binary", 10), "…th/binary");
        assert_eq!(tail_ellipsize("exact-fit!", 10), "exact-fit!");
    }

    /// The edit window follows the cursor: it stays visible with a
    /// leading `…` once the value outgrows the field.
    #[test]
    fn input_window_follows_the_cursor() {
        let rendered = |input: &Input, width: usize| -> String {
            input
                .spans(width)
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        };

        let mut input = Input::new("abcdefghij".to_owned());
        assert_eq!(rendered(&input, 6), "…ghij ", "trailing cursor cell");
        input.cursor = 0;
        assert_eq!(rendered(&input, 6), "abcdef");
    }

    /// A selection key on a focused (not yet edited) text field opens
    /// the editor and applies immediately - no Enter needed first.
    #[test]
    fn shift_selection_opens_the_editor_from_the_focused_row() {
        use crossterm::event::KeyModifiers;

        let entry = draft_for_target("pod/app", "ns", "app");
        let mut form = SettingsForm::new(entry, SERVICE_SETTINGS, " t ", "ok", validate_service);

        // The Name field is focused; shift+Home selects its whole value.
        form.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::SHIFT));
        assert!(form.typing(), "the editor opened by itself");
        form.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        form.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(form.draft.name, "x", "typing replaced the selection");
    }

    /// ⌘a selects everything and typing replaces it; shift+arrows mark
    /// part of the text for deletion.
    #[test]
    fn selection_replaces_and_deletes() {
        use crossterm::event::KeyModifiers;

        let mut input = Input::new("old value".to_owned());
        input.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::SUPER));
        input.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(input.value, "x");

        input.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        input.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
        input.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(input.value, "x", "shift+left marked `y`, backspace ate it");
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
