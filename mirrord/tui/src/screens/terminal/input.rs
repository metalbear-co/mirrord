//! Turning crossterm key events back into the bytes a terminal would have sent.
//!
//! ratatui owns stdin, so the only way to hand a keystroke to the child is to re-encode the event
//! that crossterm already parsed. The encoding is the xterm one, with two pieces of state that are
//! easy to get wrong:
//!
//! - `app_cursor` is DECCKM ([`vt100::Screen::application_cursor`]). Full-screen apps switch cursor
//!   keys to the SS3 form (`ESC O A`), and readline history or `less` navigation break if we keep
//!   sending the CSI form (`ESC [ A`) while that mode is set.
//! - Modifiers travel differently depending on the key: as an `ESC` prefix for the keys that have a
//!   single-byte encoding, but as a numeric CSI parameter for cursor, editing and function keys.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

const ESC: u8 = 0x1b;

/// Encodes a keystroke for the child, or `None` for keys a terminal does not transmit
/// (bare modifier presses, media keys, ...).
pub fn encode_key(key: KeyEvent, app_cursor: bool) -> Option<Vec<u8>> {
    let mods = Mods::from(key.modifiers);

    match key.code {
        KeyCode::Char(c) => {
            let base = match mods.ctrl.then(|| ctrl_byte(c)).flatten() {
                Some(byte) => vec![byte],
                None => c.to_string().into_bytes(),
            };
            Some(mods.esc_prefixed(base))
        }
        KeyCode::Enter => Some(mods.esc_prefixed(vec![b'\r'])),
        KeyCode::Tab => Some(mods.esc_prefixed(vec![b'\t'])),
        KeyCode::Esc => Some(mods.esc_prefixed(vec![ESC])),
        // DEL is what a terminal sends for backspace; Ctrl-Backspace is the one that sends BS.
        KeyCode::Backspace => Some(mods.esc_prefixed(vec![if mods.ctrl { 0x08 } else { 0x7f }])),
        KeyCode::BackTab => Some(b"\x1b[Z".to_vec()),

        // Option/Alt + Left and Right go out as meta-b and meta-f, the readline word-motion keys
        // that zsh, bash and everything else built on readline bind out of the box. The xterm form
        // for these (`ESC [ 1 ; 3 D`) is bound by nothing by default, so forwarding it verbatim
        // leaves the shell printing the tail of the sequence instead of jumping a word. This is
        // what macOS terminals send for the same keys, for the same reason.
        KeyCode::Left if mods.only_alt() => Some(vec![ESC, b'b']),
        KeyCode::Right if mods.only_alt() => Some(vec![ESC, b'f']),

        KeyCode::Up => Some(cursor_key(b'A', app_cursor, mods)),
        KeyCode::Down => Some(cursor_key(b'B', app_cursor, mods)),
        KeyCode::Right => Some(cursor_key(b'C', app_cursor, mods)),
        KeyCode::Left => Some(cursor_key(b'D', app_cursor, mods)),
        KeyCode::Home => Some(cursor_key(b'H', app_cursor, mods)),
        KeyCode::End => Some(cursor_key(b'F', app_cursor, mods)),

        KeyCode::Insert => Some(tilde_key(2, mods)),
        KeyCode::Delete => Some(tilde_key(3, mods)),
        KeyCode::PageUp => Some(tilde_key(5, mods)),
        KeyCode::PageDown => Some(tilde_key(6, mods)),

        KeyCode::F(n) => function_key(n, mods),

        _ => None,
    }
}

/// Wraps pasted text in the bracketed paste markers when the child asked for them, so that a
/// multi-line paste lands in the shell's editor as text instead of being run line by line.
pub fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        format!("\x1b[200~{text}\x1b[201~").into_bytes()
    } else {
        text.as_bytes().to_vec()
    }
}

#[derive(Clone, Copy)]
struct Mods {
    shift: bool,
    alt: bool,
    ctrl: bool,
}

impl From<KeyModifiers> for Mods {
    fn from(mods: KeyModifiers) -> Self {
        Self {
            shift: mods.contains(KeyModifiers::SHIFT),
            alt: mods.contains(KeyModifiers::ALT),
            ctrl: mods.contains(KeyModifiers::CONTROL),
        }
    }
}

impl Mods {
    /// The xterm modifier parameter, `None` when no modifier is held and the parameter must be
    /// omitted entirely (`ESC [ A` rather than `ESC [ 1 ; 1 A`).
    fn param(self) -> Option<u8> {
        let bits = u8::from(self.shift) | u8::from(self.alt) << 1 | u8::from(self.ctrl) << 2;
        (bits != 0).then_some(bits + 1)
    }

    /// Alt on its own. Combined with shift or control the key means something else again, and
    /// those keep the xterm encoding that can express the combination.
    fn only_alt(self) -> bool {
        self.alt && !self.shift && !self.ctrl
    }

    fn esc_prefixed(self, mut bytes: Vec<u8>) -> Vec<u8> {
        if self.alt {
            bytes.insert(0, ESC);
        }
        bytes
    }
}

fn ctrl_byte(c: char) -> Option<u8> {
    match c.to_ascii_lowercase() {
        c @ 'a'..='z' => Some(c as u8 - b'a' + 1),
        ' ' | '@' | '2' => Some(0),
        '[' | '3' => Some(27),
        '\\' | '4' => Some(28),
        ']' | '5' => Some(29),
        '^' | '6' => Some(30),
        '_' | '7' | '/' => Some(31),
        '8' | '?' => Some(0x7f),
        _ => None,
    }
}

fn cursor_key(final_byte: u8, app_cursor: bool, mods: Mods) -> Vec<u8> {
    match mods.param() {
        // A modified cursor key is always CSI, even under DECCKM.
        Some(param) => format!("\x1b[1;{param}{}", final_byte as char).into_bytes(),
        None if app_cursor => vec![ESC, b'O', final_byte],
        None => vec![ESC, b'[', final_byte],
    }
}

fn tilde_key(number: u8, mods: Mods) -> Vec<u8> {
    match mods.param() {
        Some(param) => format!("\x1b[{number};{param}~").into_bytes(),
        None => format!("\x1b[{number}~").into_bytes(),
    }
}

fn function_key(n: u8, mods: Mods) -> Option<Vec<u8>> {
    match n {
        1..=4 => {
            let final_byte = b'P' + (n - 1);
            Some(match mods.param() {
                Some(param) => format!("\x1b[1;{param}{}", final_byte as char).into_bytes(),
                None => vec![ESC, b'O', final_byte],
            })
        }
        // The arm's own range keeps the index within the eight entries.
        #[allow(clippy::indexing_slicing)]
        5..=12 => Some(tilde_key(
            [15, 17, 18, 19, 20, 21, 23, 24][n as usize - 5],
            mods,
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn plain_and_control_characters() {
        assert_eq!(
            encode_key(key(KeyCode::Char('a'), KeyModifiers::NONE), false),
            Some(b"a".to_vec())
        );
        assert_eq!(
            encode_key(key(KeyCode::Char('ä'), KeyModifiers::NONE), false),
            Some("ä".as_bytes().to_vec())
        );
        assert_eq!(
            encode_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL), false),
            Some(vec![0x03])
        );
        assert_eq!(
            encode_key(key(KeyCode::Char('b'), KeyModifiers::ALT), false),
            Some(vec![ESC, b'b'])
        );
        // A control combination the terminal has no byte for degrades to the plain character.
        assert_eq!(
            encode_key(key(KeyCode::Char('.'), KeyModifiers::CONTROL), false),
            Some(b".".to_vec())
        );
    }

    #[test]
    fn cursor_keys_follow_decckm() {
        assert_eq!(
            encode_key(key(KeyCode::Up, KeyModifiers::NONE), false),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            encode_key(key(KeyCode::Up, KeyModifiers::NONE), true),
            Some(b"\x1bOA".to_vec())
        );
        // Modifiers force the CSI form regardless of the mode.
        assert_eq!(
            encode_key(key(KeyCode::Right, KeyModifiers::SHIFT), true),
            Some(b"\x1b[1;2C".to_vec())
        );
        assert_eq!(
            encode_key(key(KeyCode::Left, KeyModifiers::CONTROL), false),
            Some(b"\x1b[1;5D".to_vec())
        );
    }

    /// The word-motion keys the shell actually binds, rather than the xterm form it ignores.
    #[test]
    fn alt_arrows_are_the_readline_word_motions() {
        assert_eq!(
            encode_key(key(KeyCode::Left, KeyModifiers::ALT), false),
            Some(b"\x1bb".to_vec()),
        );
        assert_eq!(
            encode_key(key(KeyCode::Right, KeyModifiers::ALT), false),
            Some(b"\x1bf".to_vec()),
        );
        // Application cursor mode is about the arrows themselves and does not apply here.
        assert_eq!(
            encode_key(key(KeyCode::Left, KeyModifiers::ALT), true),
            Some(b"\x1bb".to_vec()),
        );
        // Alt with another modifier is a different key again, and keeps the encoding that can
        // carry the combination.
        assert_eq!(
            encode_key(
                key(KeyCode::Left, KeyModifiers::ALT | KeyModifiers::SHIFT),
                false,
            ),
            Some(b"\x1b[1;4D".to_vec()),
        );
        // Up and Down have no word motion to map onto, so they stay as they were.
        assert_eq!(
            encode_key(key(KeyCode::Up, KeyModifiers::ALT), false),
            Some(b"\x1b[1;3A".to_vec()),
        );
    }

    #[test]
    fn editing_and_function_keys() {
        assert_eq!(
            encode_key(key(KeyCode::Delete, KeyModifiers::NONE), false),
            Some(b"\x1b[3~".to_vec())
        );
        assert_eq!(
            encode_key(key(KeyCode::F(1), KeyModifiers::NONE), false),
            Some(b"\x1bOP".to_vec())
        );
        assert_eq!(
            encode_key(key(KeyCode::F(5), KeyModifiers::NONE), false),
            Some(b"\x1b[15~".to_vec())
        );
        assert_eq!(
            encode_key(key(KeyCode::F(12), KeyModifiers::NONE), false),
            Some(b"\x1b[24~".to_vec())
        );
        assert_eq!(
            encode_key(key(KeyCode::F(13), KeyModifiers::NONE), false),
            None
        );
    }

    #[test]
    fn paste_is_bracketed_only_when_requested() {
        assert_eq!(encode_paste("hi", false), b"hi".to_vec());
        assert_eq!(encode_paste("hi", true), b"\x1b[200~hi\x1b[201~".to_vec());
    }
}
