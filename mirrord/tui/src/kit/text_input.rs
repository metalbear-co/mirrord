//! A one-line text editor shared by every box in the interface that takes typed text, so the
//! editing keys behave the same in all of them.
//!
//! It behaves like the text boxes people know from everywhere else: `Ctrl+A` selects everything,
//! `Shift` with the arrows selects, typing replaces the selection, and the mouse places the cursor
//! (click), selects (drag) and picks a word (double-click). The readline keys shells use still work
//! for everything that does not clash with those (`Ctrl+E`, `Ctrl+W`, `Ctrl+U`, `Ctrl+K`).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    style::{Modifier, Style},
    text::Span,
};

use crate::theme;

/// Drawn after the last character while the input has the keyboard.
const END_CURSOR: &str = "\u{2588}";

/// The text typed so far, where the cursor sits in it, and what is selected.
///
/// Positions count characters, not bytes, so multi-byte input never splits a character.
#[derive(Default)]
pub struct TextInput {
    value: String,
    cursor: usize,
    /// Where the selection started; the selection runs from here to the cursor.
    anchor: Option<usize>,
}

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
    /// (`Enter`, `Esc`, `Tab`, ...).
    ///
    /// Keys handled:
    /// - plain characters insert at the cursor, replacing the selection
    /// - `Backspace` / `Delete` remove the selection, or the character before / under the cursor
    /// - `Left` / `Right` move by a character, by a word with `Alt` or `Ctrl`; with `Shift` they
    ///   select
    /// - `Home` / `End` and `Ctrl+E` jump to the start / end; with `Shift` they select
    /// - `Ctrl+A` selects everything
    /// - `Alt+B` / `Alt+F` move by a word
    /// - `Ctrl+W`, `Alt+Backspace` and `Ctrl+Backspace` delete the word before the cursor
    /// - `Ctrl+U` / `Ctrl+K` delete everything before / after the cursor
    pub fn handle_key(&mut self, key: &KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        match key.code {
            KeyCode::Char('a') if ctrl => self.select_all(),
            KeyCode::Char('e') if ctrl => self.move_to(self.len(), false),
            KeyCode::Char('w') if ctrl => self.delete_word_before(),
            KeyCode::Char('u') if ctrl => self.delete_range(0, self.cursor),
            KeyCode::Char('k') if ctrl => self.delete_range(self.cursor, self.len()),
            KeyCode::Char('b') if alt => self.move_to(self.word_start(), false),
            KeyCode::Char('f') if alt => self.move_to(self.word_end(), false),
            // Any other chord is a shortcut meant for someone else, not text.
            KeyCode::Char(_) if ctrl || alt => return false,
            KeyCode::Char(char) => self.insert(&char.to_string()),
            KeyCode::Backspace if ctrl || alt => self.delete_word_before(),
            KeyCode::Backspace => match self.selection() {
                Some((from, to)) => self.delete_range(from, to),
                None if self.cursor > 0 => self.delete_range(self.cursor - 1, self.cursor),
                None => {}
            },
            KeyCode::Delete => match self.selection() {
                Some((from, to)) => self.delete_range(from, to),
                None => self.delete_range(self.cursor, (self.cursor + 1).min(self.len())),
            },
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

    /// The value as spans, with the cursor and the selection drawn in when `focused`.
    ///
    /// The cursor is a block after the last character, or the character under it drawn in
    /// `cursor` style when it sits inside the text. The selection is drawn on the brand's
    /// selection background.
    pub fn spans(&self, text: Style, cursor: Style, focused: bool) -> Vec<Span<'static>> {
        if !focused {
            return vec![Span::styled(self.value.clone(), text)];
        }
        let slice = |from: usize, to: usize| -> String {
            self.value
                .chars()
                .skip(from)
                .take(to.saturating_sub(from))
                .collect()
        };

        if let Some((from, to)) = self.selection() {
            let selected = Style::default()
                .fg(theme::LAVENDER)
                .bg(theme::FILL_DIM)
                .add_modifier(Modifier::BOLD);
            let mut spans = vec![
                Span::styled(slice(0, from), text),
                Span::styled(slice(from, to), selected),
                Span::styled(slice(to, self.len()), text),
            ];
            if self.cursor == self.len() {
                spans.push(Span::styled(END_CURSOR, cursor));
            }
            return spans;
        }

        let before = slice(0, self.cursor);
        let mut rest = self.value.chars().skip(self.cursor);
        match rest.next() {
            Some(under) => vec![
                Span::styled(before, text),
                Span::styled(under.to_string(), cursor.reversed()),
                Span::styled(rest.collect::<String>(), text),
            ],
            None => vec![Span::styled(before, text), Span::styled(END_CURSOR, cursor)],
        }
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
        if select {
            self.anchor.get_or_insert(self.cursor);
        } else {
            self.anchor = None;
        }
        self.cursor = at;
    }

    fn insert(&mut self, text: &str) {
        if let Some((from, to)) = self.selection() {
            self.delete_range(from, to);
        }
        self.anchor = None;
        let at = self.byte_index(self.cursor);
        self.value.insert_str(at, text);
        self.cursor += text.chars().count();
    }

    fn delete_range(&mut self, from: usize, to: usize) {
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
    fn paste_flattens_line_breaks() {
        let mut input = TextInput::default();
        input.paste("feature\nbranch");
        assert_eq!(input.value(), "feature branch");
    }
}
