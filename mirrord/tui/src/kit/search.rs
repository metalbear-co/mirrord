//! The `/` search box that narrows a list.
//!
//! Every list in the interface searches the same way: `/` starts typing, every keystroke narrows
//! the list, `Enter` keeps the phrase and goes back to the list, `Esc` clears it. The box stays on
//! screen while a phrase is applied, so a list is never narrowed without the user seeing why.

use crossterm::event::{Event, KeyCode};
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::{
    kit::{hints::Hint, text_input::TextInput},
    theme,
};

/// What an event did to the search.
#[derive(Debug, Eq, PartialEq)]
pub enum SearchOutcome {
    /// Not a search key; the owner should handle the event.
    Ignored,
    /// Handled, with the phrase unchanged.
    Consumed,
    /// The phrase changed, so the rows on screen did too and the owner should reset its
    /// selection.
    Changed,
}

#[derive(Default)]
pub struct SearchBox {
    input: TextInput,
    /// Set while the box has the keyboard.
    editing: bool,
}

impl SearchBox {
    /// The phrase the list is narrowed by. Empty means nothing is filtered out.
    pub fn phrase(&self) -> &str {
        self.input.value()
    }

    pub fn is_editing(&self) -> bool {
        self.editing
    }

    /// Whether the box belongs on the screen.
    pub fn visible(&self) -> bool {
        self.editing || !self.input.is_empty()
    }

    /// Whether any of `values` contains the phrase, ignoring case. Always true for an empty
    /// phrase.
    pub fn matches_any<I, S>(&self, values: I) -> bool
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        if self.input.is_empty() {
            return true;
        }
        let phrase = self.input.value().to_lowercase();
        values
            .into_iter()
            .any(|value| value.as_ref().to_lowercase().contains(&phrase))
    }

    /// Handles a key or a paste.
    ///
    /// While the box has the keyboard it takes every key, so typing `q` or `Tab` into it never
    /// quits or switches screens. Otherwise it only answers `/` and, while a phrase is applied,
    /// `Esc`.
    pub fn handle_event(&mut self, event: &Event) -> SearchOutcome {
        if self.editing {
            return self.handle_while_editing(event);
        }

        let Event::Key(key) = event else {
            return SearchOutcome::Ignored;
        };

        match key.code {
            KeyCode::Char('/') => {
                self.editing = true;
                SearchOutcome::Consumed
            }
            KeyCode::Esc if !self.input.is_empty() => {
                self.input.clear();
                SearchOutcome::Changed
            }
            _ => SearchOutcome::Ignored,
        }
    }

    fn handle_while_editing(&mut self, event: &Event) -> SearchOutcome {
        let before = self.input.value().to_owned();

        match event {
            Event::Paste(text) => self.input.paste(text),
            Event::Key(key) => match key.code {
                KeyCode::Enter => self.editing = false,
                KeyCode::Esc => {
                    self.editing = false;
                    self.input.clear();
                }
                _ => {
                    self.input.handle_key(key);
                }
            },
            _ => return SearchOutcome::Ignored,
        }

        if self.input.value() == before {
            SearchOutcome::Consumed
        } else {
            SearchOutcome::Changed
        }
    }

    /// The keys the box answers to right now.
    pub fn hints(&self) -> Vec<Hint> {
        if self.editing {
            vec![
                Hint::new("type", "to filter"),
                Hint::new("enter", "apply"),
                Hint::new("esc", "clear"),
            ]
        } else if self.input.is_empty() {
            vec![Hint::new("/", "search")]
        } else {
            vec![Hint::new("/", "search"), Hint::new("esc", "clear")]
        }
    }

    /// Draws the box: `/`, the phrase, and the cursor while it has the keyboard.
    pub fn draw(&self, frame: &mut Frame, area: Rect) {
        let mut spans = vec![Span::styled("/", theme::title())];
        spans.extend(
            self.input
                .spans(Style::default(), theme::title(), self.editing),
        );
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{Event, KeyCode, KeyEvent};

    use super::{SearchBox, SearchOutcome};

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::from(code))
    }

    #[test]
    fn typing_narrows_and_esc_clears() {
        let mut search = SearchBox::default();
        assert_eq!(
            search.handle_event(&key(KeyCode::Char('q'))),
            SearchOutcome::Ignored
        );

        assert_eq!(
            search.handle_event(&key(KeyCode::Char('/'))),
            SearchOutcome::Consumed
        );
        // `q` is text while the box has the keyboard, not "quit".
        assert_eq!(
            search.handle_event(&key(KeyCode::Char('q'))),
            SearchOutcome::Changed
        );
        assert_eq!(
            search.handle_event(&key(KeyCode::Tab)),
            SearchOutcome::Consumed
        );
        assert_eq!(search.phrase(), "q");

        assert!(search.matches_any(["Queue", "x"]));
        assert!(!search.matches_any(["pod"]));

        search.handle_event(&key(KeyCode::Enter));
        assert!(!search.is_editing());
        assert!(search.visible());

        assert_eq!(
            search.handle_event(&key(KeyCode::Esc)),
            SearchOutcome::Changed
        );
        assert!(!search.visible());
        assert!(search.matches_any(["anything"]));
    }

    #[test]
    fn paste_goes_into_the_phrase() {
        let mut search = SearchBox::default();
        search.handle_event(&key(KeyCode::Char('/')));
        assert_eq!(
            search.handle_event(&Event::Paste("staging".to_owned())),
            SearchOutcome::Changed
        );
        assert_eq!(search.phrase(), "staging");
    }
}
