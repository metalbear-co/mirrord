//! A one-line text editor shared by every box in the interface that takes typed text, so the
//! editing keys behave the same in all of them.
//!
//! It behaves like the text boxes people know from everywhere else, with the Mac keys and their
//! `Ctrl` counterparts: select all, `Shift` with any move to select, `Option`/`Ctrl` moves and
//! deletes by word, `Cmd` by line, copy, cut, paste, undo and redo, typing replaces the selection,
//! and the mouse places the cursor (click), selects (drag, `Shift`+click), picks a word
//! (double-click) or everything (triple-click). The readline keys shells use still work for
//! everything that does not clash with those (`Ctrl+E`, `Ctrl+W`, `Ctrl+U`, `Ctrl+K`, `Alt+D`).
//!
//! `Cmd` only arrives in terminals that pass it on (kitty, WezTerm, Ghostty, iTerm2 with "Report
//! keys using CSI u"); Terminal.app and a default iTerm2 keep `Cmd` shortcuts for themselves, so
//! every `Cmd` key has a `Ctrl` or `Option` twin that works everywhere.
//!
//! [`TextInput::render`] draws it as a field you can see: a filled row, or a framed box with a
//! label, the text scrolled to keep the cursor in view, and the terminal's own blinking cursor
//! where typing goes, rather than a drawn block that never blinks.

use std::{
    io::{Write, stdout},
    process::Stdio,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use mirrord_command::resolve_command;
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Paragraph},
};

use crate::theme;

/// How [`TextInput::render`] draws a field.
#[derive(Clone, Copy, Debug, Default)]
pub struct FieldLook<'a> {
    /// A short sign before the text, such as `⌕`, `:` or `$`.
    pub prefix: &'a str,
    /// Shown, muted, while the field is empty.
    pub placeholder: &'a str,
    /// Shown, muted, right after the text: the rest of a suggestion.
    pub ghost: &'a str,
    /// Shown, muted, at the right edge, such as `3 found`.
    pub note: &'a str,
    /// Draws a rounded frame titled with this around the field, which then takes three rows.
    /// Without it, the field is one filled row.
    pub label: Option<&'a str>,
    /// Whether the field has the keyboard: its frame or fill lights up and the cursor shows.
    pub focused: bool,
}

/// Where a field's text landed in the last frame, for turning mouse columns into characters.
#[derive(Clone, Copy, Debug, Default)]
struct Drawn {
    area: Rect,
    text_x: u16,
}

/// The text typed so far, where the cursor sits in it, and what is selected.
///
/// Positions count characters, not bytes, so multi-byte input never splits a character.
#[derive(Default)]
pub struct TextInput {
    value: String,
    cursor: usize,
    /// Where the selection started; the selection runs from here to the cursor.
    anchor: Option<usize>,
    /// How many characters are scrolled out on the left of the field.
    scroll: usize,
    drawn: Option<Drawn>,
    /// Earlier states to go back to with undo, newest last, and the ones undone since.
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// What the last edit was, so a run of typed characters undoes as one word.
    last_edit: Option<Edit>,
    /// The last mouse press: when, on which character, and how many presses in a row.
    last_click: Option<(Instant, usize, u8)>,
}

/// A state of the text to return to.
#[derive(Clone, Debug)]
struct Snapshot {
    value: String,
    cursor: usize,
}

/// The kind of the last edit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Edit {
    Typing,
    Other,
}

/// Whether the terminal speaks the kitty keyboard protocol, as asked once at startup.
///
/// It decides what the "super" bit on an arrow or `Backspace` means. Under the protocol it is
/// `Cmd`. Without it, the same bit comes from terminals that mark `Option` the old xterm way
/// ("meta"), and crossterm reads that bit as super too, so it has to be taken as `Option`: a word
/// move rather than a jump to the end of the line. Letters with the bit only arrive under the
/// protocol, so `Cmd+A` and the rest are unaffected.
static KEYBOARD_PROTOCOL: AtomicBool = AtomicBool::new(false);

/// Records whether the terminal speaks the kitty keyboard protocol. See [`KEYBOARD_PROTOCOL`].
pub fn set_keyboard_protocol(on: bool) {
    KEYBOARD_PROTOCOL.store(on, Ordering::Relaxed);
}

/// Most undo steps kept per text box.
const MAX_UNDO: usize = 100;

/// Presses on the same spot closer together than this count as a double- or triple-click.
const MULTI_CLICK: Duration = Duration::from_millis(400);

impl TextInput {
    pub fn value(&self) -> &str {
        &self.value
    }

    pub fn is_empty(&self) -> bool {
        self.value.is_empty()
    }

    /// Where the cursor sits, in characters from the start, for owners that scroll a long value
    /// to keep it in view.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn clear(&mut self) {
        self.value.clear();
        self.cursor = 0;
        self.anchor = None;
    }

    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.cursor = self.len();
    }

    /// The selected range, start first, when something is selected.
    fn selection(&self) -> Option<(usize, usize)> {
        let anchor = self.anchor?;
        (anchor != self.cursor).then(|| (anchor.min(self.cursor), anchor.max(self.cursor)))
    }

    pub fn has_selection(&self) -> bool {
        self.selection().is_some()
    }

    /// Applies an editing key.
    ///
    /// Returns `false` for keys that are not editing keys, so the owner can act on them
    /// (`Enter`, `Esc`, `Tab`, `Ctrl+C` with nothing selected, ...).
    ///
    /// Keys handled, `Cmd` first where a Mac has one:
    /// - characters, space included, insert at the cursor, replacing the selection
    /// - `Cmd+A` / `Ctrl+A` select everything
    /// - `Left` / `Right` by a character, `Option`/`Ctrl`/`Alt` by a word, `Cmd` to the start or
    ///   end; `Home` / `End` and `Ctrl+E` too; with `Shift`, every move selects
    /// - `Backspace` / `Delete` remove the selection or a character; with `Option`/`Ctrl` a word
    ///   (`Ctrl+W` and `Alt+D` too); `Cmd+Backspace` and `Ctrl+U` everything before the cursor,
    ///   `Ctrl+K` everything after it
    /// - `Cmd+C` (or `Ctrl+C` while something is selected) copies, `Cmd+X` / `Ctrl+X` cuts, `Cmd+V`
    ///   / `Ctrl+V` pastes what was copied here (a terminal's own paste arrives through
    ///   [`TextInput::paste`])
    /// - `Cmd+Z` / `Ctrl+Z` undo, with `Shift` redo
    pub fn handle_key(&mut self, key: &KeyEvent) -> bool {
        self.handle_key_as(key, KEYBOARD_PROTOCOL.load(Ordering::Relaxed))
    }

    /// [`TextInput::handle_key`] for a terminal that does, or does not, speak the kitty keyboard
    /// protocol.
    fn handle_key_as(&mut self, key: &KeyEvent, keyboard_protocol: bool) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let super_bit = key.modifiers.contains(KeyModifiers::SUPER);
        // See [`KEYBOARD_PROTOCOL`]: on keys other than letters, the bit is `Option` unless the
        // terminal speaks the protocol.
        let meta_is_option = !matches!(key.code, KeyCode::Char(_)) && !keyboard_protocol;
        let cmd = super_bit && !meta_is_option;
        let alt = key.modifiers.contains(KeyModifiers::ALT)
            || key.modifiers.contains(KeyModifiers::META)
            || (super_bit && meta_is_option);
        let command = cmd || ctrl;

        match key.code {
            KeyCode::Char('a' | 'A') if command => self.select_all(),
            KeyCode::Char('c' | 'C') if cmd || (ctrl && self.has_selection()) => self.copy(),
            KeyCode::Char('x' | 'X') if command => {
                self.copy();
                self.delete_selection();
            }
            KeyCode::Char('v' | 'V') if command => {
                let copied = clipboard()
                    .lock()
                    .map(|text| text.clone())
                    .unwrap_or_default();
                self.paste(&copied);
            }
            KeyCode::Char('z' | 'Z') if command && shift => self.restore(false),
            KeyCode::Char('z' | 'Z') if command => self.restore(true),
            KeyCode::Char('e') if ctrl => self.move_to(self.len(), shift),
            KeyCode::Char('w') if ctrl => self.delete_word_before(),
            KeyCode::Char('u') if ctrl => self.delete_range(0, self.cursor),
            KeyCode::Char('k') if ctrl => self.delete_range(self.cursor, self.len()),
            // `Esc b` / `Esc f` are what many Mac terminals send for Option+←/→; with Shift some
            // send them as capitals.
            KeyCode::Char(char @ ('b' | 'B')) if alt => {
                self.move_to(self.word_start(), shift || char == 'B')
            }
            KeyCode::Char(char @ ('f' | 'F')) if alt => {
                self.move_to(self.word_end(), shift || char == 'F')
            }
            KeyCode::Char('d') if alt => self.delete_word_after(),
            // Any other chord is a shortcut meant for someone else, not text.
            KeyCode::Char(_) if ctrl || alt || cmd => return false,
            KeyCode::Char(char) => self.type_char(char),
            KeyCode::Backspace if cmd => match self.selection() {
                Some((from, to)) => self.delete_range(from, to),
                None => self.delete_range(0, self.cursor),
            },
            KeyCode::Backspace if ctrl || alt => self.delete_word_before(),
            KeyCode::Backspace => match self.selection() {
                Some((from, to)) => self.delete_range(from, to),
                None if self.cursor > 0 => self.delete_range(self.cursor - 1, self.cursor),
                None => {}
            },
            KeyCode::Delete if ctrl || alt => self.delete_word_after(),
            KeyCode::Delete => match self.selection() {
                Some((from, to)) => self.delete_range(from, to),
                None => self.delete_range(self.cursor, (self.cursor + 1).min(self.len())),
            },
            KeyCode::Left if cmd => self.move_to(0, shift),
            KeyCode::Right if cmd => self.move_to(self.len(), shift),
            KeyCode::Left if ctrl || alt => self.move_to(self.word_start(), shift),
            KeyCode::Right if ctrl || alt => self.move_to(self.word_end(), shift),
            // Without Shift, Left and Right first collapse a selection to its edge.
            KeyCode::Left => match (self.selection(), shift) {
                (Some((from, _)), false) => self.move_to(from, false),
                _ => self.move_to(self.cursor.saturating_sub(1), shift),
            },
            KeyCode::Right => match (self.selection(), shift) {
                (Some((_, to)), false) => self.move_to(to, false),
                _ => self.move_to((self.cursor + 1).min(self.len()), shift),
            },
            KeyCode::Home => self.move_to(0, shift),
            KeyCode::End => self.move_to(self.len(), shift),
            _ => return false,
        }

        true
    }

    /// Inserts pasted text at the cursor, replacing the selection. Line breaks become spaces,
    /// since the input holds one line.
    pub fn paste(&mut self, text: &str) {
        let text: String = text
            .chars()
            .map(|char| if char.is_control() { ' ' } else { char })
            .collect();
        self.insert(&text);
    }

    /// A left mouse press `column` characters into the text. One press places the cursor, or with
    /// `extend` (`Shift` held) selects up to there; two in a row on the same spot select the
    /// word, three select everything.
    pub fn mouse_down(&mut self, column: usize, extend: bool) {
        let count = match self.last_click {
            Some((when, at, count)) if when.elapsed() < MULTI_CLICK && at.abs_diff(column) <= 1 => {
                count % 3 + 1
            }
            _ => 1,
        };
        self.last_click = Some((Instant::now(), column, count));
        match count {
            2 => self.select_word_at(column),
            3 => self.select_all(),
            _ if extend => self.move_to(column.min(self.len()), true),
            _ => self.click(column),
        }
    }

    /// A mouse click `column` characters into the text: the cursor goes there.
    pub fn click(&mut self, column: usize) {
        self.move_to(column.min(self.len()), false);
    }

    /// A mouse drag to `column`: selects from where the press was to there.
    pub fn drag_to(&mut self, column: usize) {
        if self.anchor.is_none() {
            self.anchor = Some(self.cursor);
        }
        self.cursor = column.min(self.len());
    }

    /// A double-click `column` characters into the text: selects the word there.
    pub fn select_word_at(&mut self, column: usize) {
        let chars: Vec<char> = self.value.chars().collect();
        let column = column.min(chars.len());
        let is_word = |at: usize| chars.get(at).is_some_and(|char| !char.is_whitespace());
        let mut start = column;
        while start > 0 && is_word(start - 1) {
            start -= 1;
        }
        let mut end = column;
        while is_word(end) {
            end += 1;
        }
        self.anchor = Some(start);
        self.cursor = end;
    }

    /// The value as spans, with the selection drawn in when `focused`, for owners that lay the
    /// text out inside a longer line. They place the terminal's cursor themselves, `cursor()`
    /// characters into the text; [`TextInput::render`] does all of it for a field of its own.
    pub fn spans(&self, text: Style, focused: bool) -> Vec<Span<'static>> {
        let Some((from, to)) = self.selection().filter(|_| focused) else {
            return vec![Span::styled(self.value.clone(), text)];
        };
        vec![
            Span::styled(self.slice(0, from), text),
            Span::styled(self.slice(from, to), selection_style()),
            Span::styled(self.slice(to, self.len()), text),
        ]
    }

    /// Draws the input as a field in `area` and, when it is focused, puts the terminal's cursor
    /// where typing goes.
    pub fn render(&mut self, frame: &mut Frame, area: Rect, field: FieldLook) {
        let row = match field.label.filter(|_| area.height >= 3) {
            Some(label) => {
                let block = Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(if field.focused {
                        theme::INDIGO
                    } else {
                        theme::BORDER_DIM
                    }))
                    .title(Span::styled(format!(" {label} "), theme::table_header()));
                let inner = block.inner(area);
                frame.render_widget(block, area);
                Rect { height: 1, ..inner }
            }
            None => {
                let row = Rect { height: 1, ..area };
                let fill = if field.focused {
                    theme::DEEP
                } else {
                    theme::INK
                };
                frame.render_widget(Block::new().style(Style::default().bg(fill)), row);
                row
            }
        };

        let prefix = if field.prefix.is_empty() {
            String::new()
        } else {
            format!("{} ", field.prefix)
        };
        let prefix_width = u16::try_from(prefix.chars().count()).unwrap_or(0);
        let note_width = u16::try_from(field.note.chars().count()).unwrap_or(0);
        // One column of padding on each side, and one between the text and the note.
        let text_x = row.x + 1 + prefix_width;
        let text_width = usize::from(
            row.width
                .saturating_sub(2 + prefix_width + note_width + u16::from(note_width > 0)),
        )
        .max(1);

        // Scroll just enough to keep the cursor, and the cell after it, in view.
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.cursor >= self.scroll + text_width {
            self.scroll = self.cursor + 1 - text_width;
        }
        self.scroll = self.scroll.min(self.len());

        let text = Style::default().fg(theme::LAVENDER);
        let mut spans = vec![Span::raw(" "), Span::styled(prefix, theme::title())];
        if self.value.is_empty() {
            spans.push(Span::styled(
                field.placeholder.to_owned(),
                theme::muted().italic(),
            ));
        } else {
            let end = (self.scroll + text_width).min(self.len());
            match self.selection().filter(|_| field.focused) {
                Some((from, to)) => {
                    let from = from.clamp(self.scroll, end);
                    let to = to.clamp(self.scroll, end);
                    spans.push(Span::styled(self.slice(self.scroll, from), text));
                    spans.push(Span::styled(self.slice(from, to), selection_style()));
                    spans.push(Span::styled(self.slice(to, end), text));
                }
                None => spans.push(Span::styled(self.slice(self.scroll, end), text)),
            }
            let shown = end - self.scroll;
            if !field.ghost.is_empty() && shown < text_width {
                let ghost: String = field.ghost.chars().take(text_width - shown).collect();
                spans.push(Span::styled(ghost, theme::muted()));
            }
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), row);
        if note_width > 0 {
            let note = Rect {
                x: row.x + row.width.saturating_sub(note_width + 1),
                width: note_width,
                ..row
            };
            frame.render_widget(
                Paragraph::new(Span::styled(field.note, theme::muted())),
                note,
            );
        }

        if field.focused {
            let column = u16::try_from(self.cursor - self.scroll).unwrap_or(0);
            frame.set_cursor_position((text_x + column, row.y));
        }
        self.drawn = Some(Drawn { area, text_x });
    }

    /// Whether a mouse position falls on the field as last drawn.
    pub fn contains(&self, column: u16, row: u16) -> bool {
        self.drawn
            .is_some_and(|drawn| drawn.area.contains(Position::new(column, row)))
    }

    /// The character under mouse column `column` in the field as last drawn.
    pub fn char_at(&self, column: u16) -> usize {
        let Some(drawn) = self.drawn else {
            return self.len();
        };
        (self.scroll + usize::from(column.saturating_sub(drawn.text_x))).min(self.len())
    }

    fn slice(&self, from: usize, to: usize) -> String {
        self.value
            .chars()
            .skip(from)
            .take(to.saturating_sub(from))
            .collect()
    }

    fn len(&self) -> usize {
        self.value.chars().count()
    }

    fn byte_index(&self, char_index: usize) -> usize {
        self.value
            .char_indices()
            .nth(char_index)
            .map_or(self.value.len(), |(index, _)| index)
    }

    /// Moves the cursor to `at`, extending the selection when `select` and dropping it otherwise.
    fn move_to(&mut self, at: usize, select: bool) {
        self.last_edit = None;
        if select {
            self.anchor.get_or_insert(self.cursor);
        } else {
            self.anchor = None;
        }
        self.cursor = at;
    }

    /// Remembers the text as it is before an edit, so undo can return to it. A run of typed
    /// characters is one step, ending at each space.
    fn remember(&mut self, edit: Edit) {
        if edit == Edit::Typing && self.last_edit == Some(Edit::Typing) {
            return;
        }
        self.last_edit = Some(edit);
        self.undo.push(Snapshot {
            value: self.value.clone(),
            cursor: self.cursor,
        });
        if self.undo.len() > MAX_UNDO {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// Goes back one step (`undo`) or forward one undone step.
    fn restore(&mut self, undo: bool) {
        let (from, to) = if undo {
            (&mut self.undo, &mut self.redo)
        } else {
            (&mut self.redo, &mut self.undo)
        };
        let Some(snapshot) = from.pop() else {
            return;
        };
        to.push(Snapshot {
            value: self.value.clone(),
            cursor: self.cursor,
        });
        self.value = snapshot.value;
        self.cursor = snapshot.cursor.min(self.len());
        self.anchor = None;
        self.last_edit = None;
    }

    fn type_char(&mut self, char: char) {
        self.remember(if char.is_whitespace() || self.has_selection() {
            Edit::Other
        } else {
            Edit::Typing
        });
        // A space ends the typed run, so the next word undoes on its own.
        if char.is_whitespace() {
            self.last_edit = None;
        }
        self.insert_text(&char.to_string());
    }

    /// Copies the selection to the interface's clipboard and to the system's.
    fn copy(&mut self) {
        let Some((from, to)) = self.selection() else {
            return;
        };
        let text = self.slice(from, to);
        if let Ok(mut copied) = clipboard().lock() {
            copied.clone_from(&text);
        }
        to_system_clipboard(&text);
    }

    fn delete_selection(&mut self) {
        if let Some((from, to)) = self.selection() {
            self.delete_range(from, to);
        }
    }

    fn delete_word_after(&mut self) {
        match self.selection() {
            Some((from, to)) => self.delete_range(from, to),
            None => self.delete_range(self.cursor, self.word_end()),
        }
    }

    fn insert(&mut self, text: &str) {
        self.remember(Edit::Other);
        self.insert_text(text);
    }

    fn insert_text(&mut self, text: &str) {
        if let Some((from, to)) = self.selection() {
            self.remove_range(from, to);
        }
        self.anchor = None;
        let at = self.byte_index(self.cursor);
        self.value.insert_str(at, text);
        self.cursor += text.chars().count();
    }

    fn delete_range(&mut self, from: usize, to: usize) {
        if from == to {
            return;
        }
        self.remember(Edit::Other);
        self.remove_range(from, to);
    }

    fn remove_range(&mut self, from: usize, to: usize) {
        let (from_byte, to_byte) = (self.byte_index(from), self.byte_index(to));
        self.value.replace_range(from_byte..to_byte, "");
        self.cursor = from;
        self.anchor = None;
    }

    fn delete_word_before(&mut self) {
        match self.selection() {
            Some((from, to)) => self.delete_range(from, to),
            None => self.delete_range(self.word_start(), self.cursor),
        }
    }

    /// Where the word before the cursor starts, skipping any whitespace right before it.
    fn word_start(&self) -> usize {
        let chars: Vec<char> = self.value.chars().take(self.cursor).collect();
        let mut at = chars.len();
        while at > 0 && chars.get(at - 1).is_some_and(|char| char.is_whitespace()) {
            at -= 1;
        }
        while at > 0 && chars.get(at - 1).is_some_and(|char| !char.is_whitespace()) {
            at -= 1;
        }
        at
    }

    /// Where the word after the cursor ends, skipping any whitespace right after it.
    fn word_end(&self) -> usize {
        let chars: Vec<char> = self.value.chars().collect();
        let mut at = self.cursor;
        while chars.get(at).is_some_and(|char| char.is_whitespace()) {
            at += 1;
        }
        while chars.get(at).is_some_and(|char| !char.is_whitespace()) {
            at += 1;
        }
        at
    }
}

/// What was last copied or cut in any text box, for pasting into another.
fn clipboard() -> &'static Mutex<String> {
    static CLIPBOARD: OnceLock<Mutex<String>> = OnceLock::new();
    CLIPBOARD.get_or_init(Mutex::default)
}

/// Puts `text` on the system clipboard: through the terminal (OSC 52, which iTerm2, kitty,
/// WezTerm, Ghostty and tmux understand, over SSH too), and through `pbcopy` on a Mac, for
/// terminals such as Terminal.app that ignore OSC 52. Best effort: a terminal that supports
/// neither leaves the text on the interface's own clipboard only.
fn to_system_clipboard(text: &str) {
    // Tests copy too, and must leave the developer's clipboard alone.
    if cfg!(test) {
        return;
    }
    let mut out = stdout();
    let _ = write!(out, "\x1b]52;c;{}\x07", base64(text.as_bytes()));
    let _ = out.flush();
    if cfg!(target_os = "macos")
        && let Ok(mut child) = resolve_command("pbcopy")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
    {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = child.wait();
    }
}

/// Standard base64, for OSC 52. Small enough not to be worth a dependency for one escape code.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let triple = chunk.iter().enumerate().fold(0u32, |all, (index, byte)| {
            all | u32::from(*byte) << (16 - 8 * index)
        });
        for index in 0..4 {
            if index <= chunk.len() {
                let six = (triple >> (18 - 6 * index)) & 0x3f;
                out.push(char::from(ALPHABET[six as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// How selected text is drawn in every field.
fn selection_style() -> Style {
    Style::default()
        .fg(theme::LAVENDER)
        .bg(theme::FILL_DIM)
        .add_modifier(Modifier::BOLD)
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::TextInput;

    fn typed(input: &mut TextInput, text: &str) {
        for char in text.chars() {
            input.handle_key(&KeyEvent::from(KeyCode::Char(char)));
        }
    }

    fn press(input: &mut TextInput, code: KeyCode, modifiers: KeyModifiers) -> bool {
        input.handle_key(&KeyEvent::new(code, modifiers))
    }

    #[test]
    fn edits_at_the_cursor() {
        let mut input = TextInput::default();
        typed(&mut input, "checkout");
        press(&mut input, KeyCode::Left, KeyModifiers::NONE);
        press(&mut input, KeyCode::Left, KeyModifiers::NONE);
        typed(&mut input, "-x");
        assert_eq!(input.value(), "checko-xut");

        press(&mut input, KeyCode::Backspace, KeyModifiers::NONE);
        press(&mut input, KeyCode::Delete, KeyModifiers::NONE);
        assert_eq!(input.value(), "checko-t");
    }

    #[test]
    fn word_motion_and_deletion() {
        let mut input = TextInput::default();
        typed(&mut input, "pod/api  default");
        press(&mut input, KeyCode::Char('w'), KeyModifiers::CONTROL);
        assert_eq!(input.value(), "pod/api  ");

        press(&mut input, KeyCode::Left, KeyModifiers::ALT);
        assert_eq!(input.cursor, 0);
        press(&mut input, KeyCode::Char('f'), KeyModifiers::ALT);
        assert_eq!(input.cursor, 7);

        press(&mut input, KeyCode::Char('k'), KeyModifiers::CONTROL);
        assert_eq!(input.value(), "pod/api");
        press(&mut input, KeyCode::Char('u'), KeyModifiers::CONTROL);
        assert!(input.is_empty());
    }

    #[test]
    fn ctrl_a_selects_everything_and_typing_replaces_it() {
        let mut input = TextInput::default();
        typed(&mut input, "checkout");
        press(&mut input, KeyCode::Char('a'), KeyModifiers::CONTROL);
        assert!(input.has_selection());
        typed(&mut input, "pay");
        assert_eq!(input.value(), "pay");
        assert!(!input.has_selection());
    }

    #[test]
    fn shift_arrows_select_and_backspace_deletes_the_selection() {
        let mut input = TextInput::default();
        typed(&mut input, "checkout-api");
        press(&mut input, KeyCode::Left, KeyModifiers::SHIFT);
        press(&mut input, KeyCode::Left, KeyModifiers::SHIFT);
        press(&mut input, KeyCode::Left, KeyModifiers::SHIFT);
        press(&mut input, KeyCode::Left, KeyModifiers::SHIFT);
        press(&mut input, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(input.value(), "checkout");

        // A plain arrow collapses a selection rather than moving past it.
        press(&mut input, KeyCode::Home, KeyModifiers::SHIFT);
        press(&mut input, KeyCode::Right, KeyModifiers::NONE);
        assert!(!input.has_selection());
        assert_eq!(input.cursor, 8);
    }

    #[test]
    fn mouse_clicks_drags_and_double_clicks() {
        let mut input = TextInput::default();
        typed(&mut input, "shop checkout");
        input.click(2);
        assert_eq!(input.cursor, 2);
        input.drag_to(4);
        typed(&mut input, "i");
        assert_eq!(input.value(), "shi checkout");

        input.select_word_at(6);
        typed(&mut input, "payments");
        assert_eq!(input.value(), "shi payments");
    }

    #[test]
    fn handles_multi_byte_characters() {
        let mut input = TextInput::default();
        typed(&mut input, "naïve");
        press(&mut input, KeyCode::Left, KeyModifiers::NONE);
        press(&mut input, KeyCode::Left, KeyModifiers::NONE);
        press(&mut input, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(input.value(), "nave");
    }

    #[test]
    fn leaves_chords_and_control_keys_to_the_owner() {
        let mut input = TextInput::default();
        assert!(!press(
            &mut input,
            KeyCode::Char('p'),
            KeyModifiers::CONTROL
        ));
        assert!(!press(&mut input, KeyCode::Enter, KeyModifiers::NONE));
        assert!(!press(&mut input, KeyCode::Esc, KeyModifiers::NONE));
        assert!(input.is_empty());
    }

    #[test]
    fn a_long_value_scrolls_and_the_real_cursor_follows_it() {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(12, 1)).unwrap();
        let mut input = TextInput::default();
        input.paste("abcdefghijklmnop");
        let field = super::FieldLook {
            prefix: ":",
            focused: true,
            ..super::FieldLook::default()
        };
        let frame = terminal
            .draw(|frame| input.render(frame, frame.area(), field))
            .unwrap();
        // One column of padding and `: ` leave 8 columns of text, ending with the cursor's cell.
        let row: String = (0..12)
            .map(|x| frame.buffer[(x, 0)].symbol().to_owned())
            .collect();
        assert_eq!(row, " : jklmnop  ");
        terminal.backend_mut().assert_cursor_position((10, 0));
        // A click on the `l` lands on the `l`.
        assert_eq!(input.char_at(5), 11);
    }

    /// Under the kitty keyboard protocol, the super bit is `Cmd`.
    #[test]
    fn mac_command_keys_move_select_and_delete_by_line() {
        let mut input = TextInput::default();
        let cmd = |input: &mut TextInput, code, modifiers| {
            input.handle_key_as(&KeyEvent::new(code, modifiers), true)
        };
        typed(&mut input, "shop checkout");
        cmd(
            &mut input,
            KeyCode::Left,
            KeyModifiers::SUPER | KeyModifiers::SHIFT,
        );
        assert_eq!(input.selection(), Some((0, 13)));
        cmd(&mut input, KeyCode::Right, KeyModifiers::SUPER);
        assert_eq!(input.cursor, 13);
        cmd(&mut input, KeyCode::Char('a'), KeyModifiers::SUPER);
        typed(&mut input, "payments");
        assert_eq!(input.value(), "payments");
        cmd(&mut input, KeyCode::Backspace, KeyModifiers::SUPER);
        assert!(input.is_empty());
    }

    /// Without the protocol, the same bit on an arrow is a terminal marking `Option` the old
    /// "meta" way: a word, not the whole line. `Cmd+A` still selects everything.
    #[test]
    fn without_the_protocol_the_meta_bit_on_arrows_is_option() {
        let mut input = TextInput::default();
        let old = |input: &mut TextInput, code, modifiers| {
            input.handle_key_as(&KeyEvent::new(code, modifiers), false)
        };
        typed(&mut input, "shop checkout");
        old(
            &mut input,
            KeyCode::Left,
            KeyModifiers::SUPER | KeyModifiers::SHIFT,
        );
        assert_eq!(input.selection(), Some((5, 13)));
        old(&mut input, KeyCode::Char('a'), KeyModifiers::SUPER);
        assert_eq!(input.selection(), Some((0, 13)));
    }

    /// Many Mac terminals send Option+←/→ as `Esc b` / `Esc f`, and with Shift as capitals.
    #[test]
    fn escape_b_and_f_move_and_select_by_word() {
        let mut input = TextInput::default();
        typed(&mut input, "shop checkout");
        press(&mut input, KeyCode::Char('B'), KeyModifiers::ALT);
        assert_eq!(input.selection(), Some((5, 13)));
        press(&mut input, KeyCode::Char('b'), KeyModifiers::ALT);
        assert_eq!((input.cursor, input.has_selection()), (0, false));
        press(
            &mut input,
            KeyCode::Char('f'),
            KeyModifiers::ALT | KeyModifiers::SHIFT,
        );
        assert_eq!(input.selection(), Some((0, 4)));
    }

    /// Option+Shift+arrows the way most terminals send them (alt and shift bits).
    #[test]
    fn option_shift_arrows_select_word_by_word() {
        let mut input = TextInput::default();
        typed(&mut input, "pod api default");
        let option_shift = KeyModifiers::ALT | KeyModifiers::SHIFT;
        press(&mut input, KeyCode::Left, option_shift);
        press(&mut input, KeyCode::Left, option_shift);
        assert_eq!(input.selection(), Some((4, 15)));
        press(&mut input, KeyCode::Home, KeyModifiers::NONE);
        press(&mut input, KeyCode::Right, option_shift);
        assert_eq!(input.selection(), Some((0, 3)));
        press(&mut input, KeyCode::Right, KeyModifiers::NONE);
        assert_eq!((input.cursor, input.has_selection()), (3, false));
    }

    #[test]
    fn option_selects_and_deletes_words() {
        let mut input = TextInput::default();
        typed(&mut input, "pod api default");
        press(
            &mut input,
            KeyCode::Left,
            KeyModifiers::ALT | KeyModifiers::SHIFT,
        );
        press(&mut input, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(input.value(), "pod api ");
        press(&mut input, KeyCode::Home, KeyModifiers::NONE);
        press(&mut input, KeyCode::Delete, KeyModifiers::ALT);
        assert_eq!(input.value(), " api ");
    }

    #[test]
    fn undo_steps_back_a_word_at_a_time_and_redo_returns() {
        let mut input = TextInput::default();
        typed(&mut input, "npm run dev");
        press(&mut input, KeyCode::Char('z'), KeyModifiers::CONTROL);
        assert_eq!(input.value(), "npm run ");
        press(&mut input, KeyCode::Char('z'), KeyModifiers::SUPER);
        assert_eq!(input.value(), "npm run");
        press(
            &mut input,
            KeyCode::Char('z'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        assert_eq!(input.value(), "npm run ");
    }

    #[test]
    fn cut_and_paste_move_text_between_boxes() {
        let mut from = TextInput::default();
        typed(&mut from, "checkout");
        press(&mut from, KeyCode::Char('a'), KeyModifiers::CONTROL);
        press(&mut from, KeyCode::Char('x'), KeyModifiers::CONTROL);
        assert!(from.is_empty());

        let mut to = TextInput::default();
        press(&mut to, KeyCode::Char('v'), KeyModifiers::SUPER);
        assert_eq!(to.value(), "checkout");
        // With nothing selected, Ctrl+C is left to the application (it quits).
        assert!(!press(&mut to, KeyCode::Char('c'), KeyModifiers::CONTROL));
    }

    #[test]
    fn presses_in_a_row_select_a_word_then_everything() {
        let mut input = TextInput::default();
        typed(&mut input, "shop checkout");
        input.mouse_down(7, false);
        input.mouse_down(7, false);
        typed(&mut input, "x");
        assert_eq!(input.value(), "shop x");
        input.mouse_down(1, false);
        input.mouse_down(1, false);
        input.mouse_down(1, false);
        typed(&mut input, "y");
        assert_eq!(input.value(), "y");
        // Shift+click selects from the cursor to where the press landed.
        typed(&mut input, "abcd");
        input.last_click = None;
        input.mouse_down(1, true);
        typed(&mut input, "Z");
        assert_eq!(input.value(), "yZ");
    }

    #[test]
    fn base64_matches_the_standard_alphabet_and_padding() {
        assert_eq!(super::base64(b"mirrord"), "bWlycm9yZA==");
        assert_eq!(super::base64(b"abc"), "YWJj");
        assert_eq!(super::base64(b""), "");
    }

    #[test]
    fn paste_flattens_line_breaks() {
        let mut input = TextInput::default();
        input.paste("feature\nbranch");
        assert_eq!(input.value(), "feature branch");
    }
}
