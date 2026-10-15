//! A one-line text editor shared by every box in the interface that takes typed text, so the
//! editing keys behave the same in all of them.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{style::Style, text::Span};

/// Drawn after the last character while the input has the keyboard.
const END_CURSOR: &str = "\u{2588}";

/// The text typed so far and where the cursor sits in it.
///
/// The cursor counts characters, not bytes, so multi-byte input never splits a character.
#[derive(Default)]
pub struct TextInput {
    value: String,
    cursor: usize,
}

impl TextInput {
    pub fn value(&self) -> &str {
        &self.value
    }

    pub fn is_empty(&self) -> bool {
        self.value.is_empty()
    }

    pub fn clear(&mut self) {
        self.value.clear();
        self.cursor = 0;
    }

    /// Applies an editing key.
    ///
    /// Returns `false` for keys that are not editing keys, so the owner can act on them
    /// (`Enter`, `Esc`, `Tab`, ...).
    ///
    /// Keys handled, matching what readline-based shells bind by default:
    /// - plain characters insert at the cursor
    /// - `Backspace` / `Delete` remove the character before / under the cursor
    /// - `Left` / `Right` move by a character, by a word with `Alt` or `Ctrl`
    /// - `Home` / `End` and `Ctrl+A` / `Ctrl+E` jump to the start / end
    /// - `Alt+B` / `Alt+F` move by a word
    /// - `Ctrl+W` deletes the word before the cursor
    /// - `Ctrl+U` / `Ctrl+K` delete everything before / after the cursor
    pub fn handle_key(&mut self, key: &KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);

        match key.code {
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::Char('e') if ctrl => self.cursor = self.len(),
            KeyCode::Char('w') if ctrl => self.delete_range(self.word_start(), self.cursor),
            KeyCode::Char('u') if ctrl => self.delete_range(0, self.cursor),
            KeyCode::Char('k') if ctrl => self.delete_range(self.cursor, self.len()),
            KeyCode::Char('b') if alt => self.cursor = self.word_start(),
            KeyCode::Char('f') if alt => self.cursor = self.word_end(),
            // Any other chord is a shortcut meant for someone else, not text.
            KeyCode::Char(_) if ctrl || alt => return false,
            KeyCode::Char(char) => self.insert(&char.to_string()),
            KeyCode::Backspace if self.cursor > 0 => {
                self.delete_range(self.cursor - 1, self.cursor)
            }
            KeyCode::Backspace => {}
            KeyCode::Delete => self.delete_range(self.cursor, (self.cursor + 1).min(self.len())),
            KeyCode::Left if ctrl || alt => self.cursor = self.word_start(),
            KeyCode::Right if ctrl || alt => self.cursor = self.word_end(),
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.len()),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.len(),
            _ => return false,
        }

        true
    }

    /// Inserts pasted text at the cursor. Line breaks become spaces, since the input holds one
    /// line.
    pub fn paste(&mut self, text: &str) {
        let text: String = text
            .chars()
            .map(|char| if char.is_control() { ' ' } else { char })
            .collect();
        self.insert(&text);
    }

    /// The value as spans, with the cursor drawn in when `focused`.
    ///
    /// The cursor is a block after the last character, or the character under it drawn in
    /// `cursor` style when it sits inside the text.
    pub fn spans(&self, text: Style, cursor: Style, focused: bool) -> Vec<Span<'static>> {
        if !focused {
            return vec![Span::styled(self.value.clone(), text)];
        }

        let before: String = self.value.chars().take(self.cursor).collect();
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

    fn insert(&mut self, text: &str) {
        let at = self.byte_index(self.cursor);
        self.value.insert_str(at, text);
        self.cursor += text.chars().count();
    }

    fn delete_range(&mut self, from: usize, to: usize) {
        let (from_byte, to_byte) = (self.byte_index(from), self.byte_index(to));
        self.value.replace_range(from_byte..to_byte, "");
        self.cursor = from;
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
