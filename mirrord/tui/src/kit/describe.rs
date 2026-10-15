//! A resource shown whole, as YAML: what `Enter` opens on a row, like `kubectl get -o yaml` but
//! scrollable and searchable in place.

use crossterm::event::{Event, KeyCode};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use serde::Serialize;

use crate::{
    kit::{
        hints::Hint,
        panel,
        search::{SearchBox, SearchOutcome},
    },
    theme,
};

/// Lines moved by `PageUp` / `PageDown`.
const PAGE: usize = 20;

pub struct Describe {
    /// What is described, such as `session a3f9c1`.
    title: String,
    lines: Vec<String>,
    /// First line on screen.
    scroll: usize,
    /// Lines the view had room for when last drawn, for paging and clamping.
    height: usize,
    search: SearchBox,
}

/// What an event did to the view.
pub enum DescribeOutcome {
    Open,
    /// The user went back to the list.
    Closed,
}

impl Describe {
    /// Describes `value`, serialized as YAML. A value that cannot be serialized says so instead.
    pub fn new(title: String, value: &impl Serialize) -> Self {
        let text = serde_saphyr::to_string(value)
            .unwrap_or_else(|error| format!("# could not show this resource: {error}"));
        Self {
            title,
            lines: text.lines().map(str::to_owned).collect(),
            scroll: 0,
            height: 1,
            search: SearchBox::default(),
        }
    }

    pub fn handle_event(&mut self, event: &Event) -> DescribeOutcome {
        match self.search.handle_event(event) {
            SearchOutcome::Changed => {
                self.jump_to_match(0);
                return DescribeOutcome::Open;
            }
            SearchOutcome::Consumed => return DescribeOutcome::Open,
            SearchOutcome::Ignored => {}
        }

        let Event::Key(key) = event else {
            return DescribeOutcome::Open;
        };
        let last = self.lines.len().saturating_sub(self.height);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return DescribeOutcome::Closed,
            KeyCode::Down | KeyCode::Char('j') => self.scroll = (self.scroll + 1).min(last),
            KeyCode::Up | KeyCode::Char('k') => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::PageDown | KeyCode::Char(' ') => self.scroll = (self.scroll + PAGE).min(last),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(PAGE),
            KeyCode::Home | KeyCode::Char('g') => self.scroll = 0,
            KeyCode::End | KeyCode::Char('G') => self.scroll = last,
            KeyCode::Char('n') => self.jump_to_match(self.scroll + 1),
            _ => {}
        }
        DescribeOutcome::Open
    }

    /// Scrolls to the first line at or after `from` containing the search phrase, wrapping around.
    fn jump_to_match(&mut self, from: usize) {
        if self.search.phrase().is_empty() || self.lines.is_empty() {
            return;
        }
        let count = self.lines.len();
        let found = (0..count)
            .map(|offset| (from + offset) % count)
            .find(|index| {
                self.lines
                    .get(*index)
                    .is_some_and(|line| self.search.matches_any([line]))
            });
        if let Some(index) = found {
            self.scroll = index.min(count.saturating_sub(self.height));
        }
    }

    pub fn hints(&self) -> Vec<Hint> {
        if self.search.is_editing() {
            return self.search.hints();
        }
        let mut hints = vec![
            Hint::new("\u{2191}/\u{2193}", "scroll"),
            Hint::new("esc", "back"),
        ];
        hints.extend(self.search.hints());
        if !self.search.phrase().is_empty() {
            hints.push(Hint::new("n", "next match"));
        }
        hints
    }

    pub fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let search_height = u16::from(self.search.visible());
        let [body, search] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(search_height)]).areas(area);

        let position = format!(
            "{}-{} of {}",
            (self.scroll + 1).min(self.lines.len()),
            (self.scroll + self.height).min(self.lines.len()),
            self.lines.len()
        );
        let block = panel::block(
            &format!("describe({})", self.title),
            None,
            Some(&position),
            None,
        );
        let inner = block.inner(body);
        frame.render_widget(block, body);

        self.height = usize::from(inner.height).max(1);
        self.scroll = self
            .scroll
            .min(self.lines.len().saturating_sub(self.height));

        let lines: Vec<Line> = self
            .lines
            .iter()
            .skip(self.scroll)
            .take(self.height)
            .map(|line| {
                let matched = !self.search.phrase().is_empty() && self.search.matches_any([line]);
                yaml_line(line, matched)
            })
            .collect();
        frame.render_widget(Paragraph::new(lines), inner);

        if search_height > 0 {
            self.search.draw(frame, search);
        }
    }
}

/// One YAML line, the key tinted so the structure reads at a glance, and the whole line marked
/// when it matches the search.
fn yaml_line(line: &str, matched: bool) -> Line<'static> {
    let key_style = Style::default().fg(theme::INDIGO_LIGHT);
    let mut value_style = Style::default().fg(theme::LAVENDER);
    if matched {
        value_style = value_style.fg(theme::AMBER).add_modifier(Modifier::BOLD);
    }

    let body = line.trim_start();
    let indent = line.get(..line.len() - body.len()).unwrap_or_default();
    let (dash, rest) = match body.strip_prefix("- ") {
        Some(rest) => ("- ", rest),
        None => ("", body),
    };

    // `key: value` splits at the first `: ` (or a trailing `:`), which is where YAML puts it;
    // anything else is a plain value.
    let split = rest
        .find(": ")
        .or_else(|| rest.ends_with(':').then(|| rest.len() - 1))
        .filter(|at| {
            rest.get(..*at).is_some_and(|key| !key.contains(' ')) || rest.starts_with('"')
        });

    let mut spans = vec![
        Span::raw(indent.to_owned()),
        Span::styled(dash, theme::muted()),
    ];
    match split {
        Some(at) => {
            let mut key = key_style;
            if matched {
                key = key.fg(theme::AMBER).add_modifier(Modifier::BOLD);
            }
            let (name, value) = rest.split_at(at);
            spans.push(Span::styled(name.to_owned(), key));
            spans.push(Span::styled(value.to_owned(), value_style));
        }
        None => spans.push(Span::styled(rest.to_owned(), value_style)),
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use crossterm::event::{Event, KeyCode, KeyEvent};
    use serde_json::json;

    use super::{Describe, DescribeOutcome};

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::from(code))
    }

    #[test]
    fn search_jumps_to_the_matching_line_and_esc_goes_back() {
        let value =
            json!({ "spec": { "session": { "user": "vlad", "target": "deploy/checkout" } } });
        let mut describe = Describe::new("session a3f9c1".to_owned(), &value);
        assert!(
            describe
                .lines
                .iter()
                .any(|line| line.contains("target: deploy/checkout"))
        );

        describe.handle_event(&key(KeyCode::Char('/')));
        for char in "target".chars() {
            describe.handle_event(&key(KeyCode::Char(char)));
        }
        let line = describe
            .lines
            .get(describe.scroll)
            .expect("scrolled onto a line");
        assert!(line.contains("target"), "{line}");

        describe.handle_event(&key(KeyCode::Enter));
        assert!(matches!(
            describe.handle_event(&key(KeyCode::Esc)),
            DescribeOutcome::Open
        ));
        assert!(matches!(
            describe.handle_event(&key(KeyCode::Esc)),
            DescribeOutcome::Closed
        ));
    }
}
