//! The `:` command line: type a resource or a command, with suggestions as you type.
//!
//! `:sessions` switches to sessions, `:ns staging` to a namespace, `:ctx` opens the context
//! picker. `Tab` completes the highlighted suggestion and `Enter` runs it, so `:se<Enter>` is
//! enough to reach sessions.

use crossterm::event::{Event, KeyCode};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Clear, Paragraph},
};

use crate::{helpers::ellipsize, kit::text_input::TextInput, theme};

/// Most suggestions shown at once.
const MAX_SUGGESTIONS: usize = 8;

/// Width of the suggestion list.
const LIST_WIDTH: u16 = 56;

/// One command the bar knows.
pub struct Entry<T> {
    /// What can be typed for it, the first being the name shown.
    pub names: Vec<String>,
    pub about: &'static str,
    pub action: T,
    /// Values its argument can take, offered once a space follows the name (namespaces for
    /// `ns`). Empty for commands without an argument.
    pub args: Vec<String>,
}

/// What an event did to the bar.
pub enum Outcome<T> {
    Open,
    Closed,
    /// Run `T`, with the argument typed after its name, if any.
    Run(T, Option<String>),
}

pub struct CommandBar<T> {
    entries: Vec<Entry<T>>,
    input: TextInput,
    /// Index of the highlighted suggestion.
    selected: usize,
}

/// A suggestion, and what `Tab` turns the input into for it.
struct Suggestion<'a> {
    label: &'a str,
    about: &'a str,
    completion: String,
}

impl<T: Copy> CommandBar<T> {
    pub fn new(entries: Vec<Entry<T>>) -> Self {
        Self {
            entries,
            input: TextInput::default(),
            selected: 0,
        }
    }

    /// The name typed and the argument after it, if a space was typed.
    fn parts(&self) -> (&str, Option<&str>) {
        let text = self.input.value().trim_start();
        match text.split_once(' ') {
            Some((name, arg)) => (name, Some(arg.trim())),
            None => (text, None),
        }
    }

    fn entry_named(&self, name: &str) -> Option<&Entry<T>> {
        self.entries
            .iter()
            .find(|entry| entry.names.iter().any(|candidate| candidate == name))
    }

    fn suggestions(&self) -> Vec<Suggestion<'_>> {
        let (name, arg) = self.parts();

        if let Some(arg) = arg {
            let Some(entry) = self.entry_named(name) else {
                return Vec::new();
            };
            let mut matching: Vec<&String> = entry
                .args
                .iter()
                .filter(|value| value.starts_with(arg))
                .collect();
            matching.extend(
                entry
                    .args
                    .iter()
                    .filter(|value| !value.starts_with(arg) && value.contains(arg)),
            );
            return matching
                .into_iter()
                .take(MAX_SUGGESTIONS)
                .map(|value| Suggestion {
                    label: value,
                    about: "",
                    completion: format!("{name} {value}"),
                })
                .collect();
        }

        // Commands whose name starts with what was typed come first, then those that merely
        // contain it.
        let starts = |entry: &&Entry<T>| {
            entry
                .names
                .iter()
                .any(|candidate| candidate.starts_with(name))
        };
        let contains =
            |entry: &&Entry<T>| entry.names.iter().any(|candidate| candidate.contains(name));
        self.entries
            .iter()
            .filter(starts)
            .chain(
                self.entries
                    .iter()
                    .filter(|entry| !starts(entry) && contains(entry)),
            )
            .take(MAX_SUGGESTIONS)
            .map(|entry| {
                let label = entry
                    .names
                    .iter()
                    .find(|candidate| candidate.starts_with(name))
                    .or(entry.names.first())
                    .map(String::as_str)
                    .unwrap_or_default();
                Suggestion {
                    label,
                    about: entry.about,
                    completion: label.to_owned(),
                }
            })
            .collect()
    }

    pub fn handle_event(&mut self, event: &Event) -> Outcome<T> {
        let key = match event {
            Event::Paste(text) => {
                self.input.paste(text);
                self.selected = 0;
                return Outcome::Open;
            }
            Event::Key(key) => key,
            _ => return Outcome::Open,
        };

        match key.code {
            KeyCode::Esc => return Outcome::Closed,
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => {
                let count = self.suggestions().len();
                self.selected = (self.selected + 1).min(count.saturating_sub(1));
            }
            KeyCode::Tab => self.complete(),
            KeyCode::Enter => return self.run(),
            _ => {
                if self.input.handle_key(key) {
                    self.selected = 0;
                }
            }
        }
        Outcome::Open
    }

    /// Replaces the input with the highlighted suggestion, adding the space that starts an
    /// argument when the command takes one.
    fn complete(&mut self) {
        let Some(completion) = self
            .suggestions()
            .into_iter()
            .nth(self.selected)
            .map(|suggestion| suggestion.completion)
        else {
            return;
        };
        let takes_arg = self
            .entry_named(&completion)
            .is_some_and(|entry| !entry.args.is_empty());
        self.input.clear();
        self.input.paste(&completion);
        if takes_arg {
            self.input.paste(" ");
        }
        self.selected = 0;
    }

    fn run(&mut self) -> Outcome<T> {
        let (name, arg) = self.parts();
        if let Some(entry) = self.entry_named(name) {
            let arg = arg.filter(|arg| !arg.is_empty()).map(str::to_owned);
            // `:ns sta<Enter>` means the namespace suggested for `sta`, not one named `sta`.
            let arg = match (&arg, self.suggestions().into_iter().nth(self.selected)) {
                (Some(typed), Some(suggestion)) if !entry.args.contains(typed) => suggestion
                    .completion
                    .split_once(' ')
                    .map(|(_, value)| value.to_owned()),
                _ => arg,
            };
            return Outcome::Run(entry.action, arg);
        }

        let Some(completion) = self
            .suggestions()
            .into_iter()
            .nth(self.selected)
            .map(|suggestion| suggestion.completion)
        else {
            return Outcome::Open;
        };
        match self.entry_named(&completion) {
            Some(entry) => Outcome::Run(entry.action, None),
            None => Outcome::Open,
        }
    }

    /// Draws the input line into `bar`, and the suggestions in a list just above it.
    pub fn draw(&self, frame: &mut Frame, bar: Rect) {
        let suggestions = self.suggestions();

        let mut spans = vec![Span::styled(":", theme::title())];
        spans.extend(
            self.input
                .spans(Style::default().fg(theme::LAVENDER), theme::title(), true),
        );
        // The rest of the highlighted suggestion, ghosted after the cursor.
        if let Some(suggestion) = suggestions.get(self.selected)
            && let Some(rest) = suggestion.completion.strip_prefix(self.input.value())
        {
            spans.push(Span::styled(rest.to_owned(), theme::muted()));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), bar);

        if suggestions.is_empty() {
            return;
        }
        let height = suggestions.len() as u16 + 2;
        let width = LIST_WIDTH.min(bar.width);
        let list = Rect {
            x: bar.x,
            y: bar.y.saturating_sub(height),
            width,
            height: height.min(bar.y),
        };
        let inner_width = usize::from(width.saturating_sub(2));
        let lines: Vec<Line> = suggestions
            .iter()
            .enumerate()
            .map(|(index, suggestion)| {
                let text = ellipsize(
                    &format!(" {:<14} {}", suggestion.label, suggestion.about),
                    inner_width,
                );
                let padded = format!("{text:<inner_width$}");
                if index == self.selected {
                    Line::styled(padded, theme::selection())
                } else {
                    Line::from_iter([Span::styled(padded, Style::default().fg(theme::LAVENDER))])
                }
            })
            .collect();

        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(
                Style::default()
                    .fg(theme::INDIGO)
                    .add_modifier(Modifier::BOLD),
            );
        let inner = block.inner(list);
        frame.render_widget(Clear, list);
        frame.render_widget(block, list);
        frame.render_widget(Paragraph::new(lines), inner);
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{Event, KeyCode, KeyEvent};

    use super::{CommandBar, Entry, Outcome};

    fn bar() -> CommandBar<u8> {
        CommandBar::new(vec![
            Entry {
                names: vec!["sessions".to_owned(), "ss".to_owned()],
                about: "mirrord sessions",
                action: 1,
                args: Vec::new(),
            },
            Entry {
                names: vec!["ns".to_owned(), "namespace".to_owned()],
                about: "switch namespace",
                action: 2,
                args: vec!["all".to_owned(), "staging".to_owned(), "prod".to_owned()],
            },
        ])
    }

    fn typed(bar: &mut CommandBar<u8>, text: &str) -> Outcome<u8> {
        let mut outcome = Outcome::Open;
        for char in text.chars() {
            let code = match char {
                '\n' => KeyCode::Enter,
                '\t' => KeyCode::Tab,
                char => KeyCode::Char(char),
            };
            outcome = bar.handle_event(&Event::Key(KeyEvent::from(code)));
        }
        outcome
    }

    #[test]
    fn a_prefix_runs_the_command_it_suggests() {
        assert!(matches!(typed(&mut bar(), "se\n"), Outcome::Run(1, None)));
        assert!(matches!(typed(&mut bar(), "ss\n"), Outcome::Run(1, None)));
    }

    #[test]
    fn arguments_complete_from_the_values_offered() {
        let mut bar = bar();
        typed(&mut bar, "ns\t");
        assert_eq!(bar.input.value(), "ns ");
        match typed(&mut bar, "sta\n") {
            Outcome::Run(2, Some(arg)) => assert_eq!(arg, "staging"),
            _ => panic!("expected the namespace command to run with `staging`"),
        }
        assert!(matches!(
            typed(&mut super::tests::bar(), "ns\n"),
            Outcome::Run(2, None)
        ));
    }
}
