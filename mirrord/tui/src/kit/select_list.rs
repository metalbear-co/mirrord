//! A searchable list to pick from, with the same keys, look and mouse handling wherever the
//! interface asks the user to choose things.
//!
//! Every row shows whether it is picked, always in the same column: `[✓]` / `[ ]` where several
//! can be picked, `(●)` / `( )` where one is. `Space` or a click picks and unpicks, typing
//! searches (the matching text lights up), and `Enter` means "done, go on". Plain letters always
//! go to the search, so owners put their own actions on `Ctrl` chords. The list does not own
//! what is picked: the owner says what is picked when drawing and acts on the [`ListEvent`]s, so
//! a pick can outlive the rows on screen (a folder ticked in one directory stays ticked after
//! moving to another).

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::{helpers::ellipsize, kit::text_input::TextInput, theme};

/// Two clicks on one row closer together than this are a double-click.
pub const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// Columns before the search text: `› ` and a space.
const SEARCH_PREFIX: u16 = 2;

/// How many can be picked, which decides the look of the pick column.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Picks {
    Many,
    One,
}

/// One row.
#[derive(Clone, Debug, Default)]
pub struct Choice {
    /// What the owner knows the row by.
    pub id: String,
    /// A heading drawn above the first row of each run of rows with the same one.
    pub section: Option<&'static str>,
    /// A short sign before the title, such as `★` for a suggestion.
    pub mark: Option<Span<'static>>,
    pub title: String,
    /// The title's colour; lavender when unset.
    pub tone: Option<Color>,
    /// Shown muted after the title.
    pub detail: String,
    /// Shown at the right edge.
    pub note: Vec<Span<'static>>,
    /// More words a search can match, beyond the title and the detail.
    pub keywords: String,
    /// Rows that act rather than get picked (such as `browse your home folder`) have no pick
    /// column.
    pub pickable: bool,
}

/// What a key press or click asked for.
#[derive(Clone, Debug, PartialEq)]
pub enum ListEvent {
    Nothing,
    /// The cursor or the search changed; nothing for the owner to do but redraw.
    Moved,
    /// Pick or unpick this row.
    Toggle(String),
    /// Go into this row (`→` or a double-click), for rows that lead somewhere.
    Open(String),
    /// `Enter`: done here, go on.
    Next,
    /// `Esc` with nothing typed: go back.
    Back,
    /// `←`, or `Backspace` with nothing typed: go up a level, for lists that have levels.
    Up,
    /// `Ctrl+A` with nothing typed: pick everything shown, or nothing if all of it is picked.
    ToggleAll,
}

pub struct SelectList {
    picks: Picks,
    placeholder: &'static str,
    filter: TextInput,
    choices: Vec<Choice>,
    /// Indices into `choices` that match the search, in the order shown.
    shown: Vec<usize>,
    /// Index into `shown`.
    cursor: usize,
    /// Where the search line and each drawn row landed in the last frame, for mouse hits.
    search_area: Rect,
    rows: Vec<(u16, usize)>,
    list_area: Rect,
    last_click: Option<(Instant, usize)>,
}

impl SelectList {
    pub fn new(picks: Picks, placeholder: &'static str) -> Self {
        Self {
            picks,
            placeholder,
            filter: TextInput::default(),
            choices: Vec::new(),
            shown: Vec::new(),
            cursor: 0,
            search_area: Rect::default(),
            rows: Vec::new(),
            list_area: Rect::default(),
            last_click: None,
        }
    }

    /// Replaces the rows, keeping the cursor on the same row when it is still there.
    pub fn set_choices(&mut self, choices: Vec<Choice>) {
        let focused = self.focused().map(|choice| choice.id.clone());
        self.choices = choices;
        self.refilter();
        if let Some(id) = focused {
            self.focus(&id);
        }
    }

    pub fn focused(&self) -> Option<&Choice> {
        self.shown
            .get(self.cursor)
            .and_then(|index| self.choices.get(*index))
    }

    /// Puts the cursor on the row known as `id`, if it is shown.
    pub fn focus(&mut self, id: &str) {
        if let Some(at) = self.shown.iter().position(|index| {
            self.choices
                .get(*index)
                .is_some_and(|choice| choice.id == id)
        }) {
            self.cursor = at;
        }
    }

    pub fn search(&self) -> &str {
        self.filter.value()
    }

    pub fn clear_search(&mut self) {
        self.filter.clear();
        self.refilter();
    }

    /// The ids of the pickable rows the search shows.
    pub fn shown_ids(&self) -> Vec<String> {
        self.shown
            .iter()
            .filter_map(|index| self.choices.get(*index))
            .filter(|choice| choice.pickable)
            .map(|choice| choice.id.clone())
            .collect()
    }

    /// Rows whose title, detail or keywords hold every typed word, the ones matching in the title
    /// first, otherwise in the owner's order.
    fn refilter(&mut self) {
        let words = words(self.filter.value());
        let mut shown: Vec<(bool, usize)> = self
            .choices
            .iter()
            .enumerate()
            .filter_map(|(index, choice)| {
                let title = choice.title.to_lowercase();
                let all = format!(
                    "{title} {} {}",
                    choice.detail.to_lowercase(),
                    choice.keywords.to_lowercase()
                );
                words
                    .iter()
                    .all(|word| all.contains(word.as_str()))
                    .then(|| {
                        let in_title = words.iter().all(|word| title.contains(word.as_str()));
                        (!in_title, index)
                    })
            })
            .collect();
        if !words.is_empty() {
            shown.sort_by_key(|(later, _)| *later);
        }
        self.shown = shown.into_iter().map(|(_, index)| index).collect();
        self.cursor = self.cursor.min(self.shown.len().saturating_sub(1));
    }

    fn move_by(&mut self, delta: isize) {
        let last = self.shown.len().saturating_sub(1);
        self.cursor = self.cursor.saturating_add_signed(delta).min(last);
    }

    fn focused_id(&self) -> Option<String> {
        self.focused().map(|choice| choice.id.clone())
    }

    pub fn handle_key(&mut self, key: &KeyEvent) -> ListEvent {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let page = isize::try_from(self.list_area.height.max(1)).unwrap_or(1);
        match key.code {
            KeyCode::Up => self.move_by(-1),
            KeyCode::Down => self.move_by(1),
            KeyCode::Char('p') if ctrl => self.move_by(-1),
            KeyCode::Char('n') if ctrl => self.move_by(1),
            KeyCode::PageUp => self.move_by(-page),
            KeyCode::PageDown => self.move_by(page),
            KeyCode::Char(' ') => {
                return match self.focused() {
                    Some(choice) if choice.pickable => ListEvent::Toggle(choice.id.clone()),
                    _ => ListEvent::Nothing,
                };
            }
            KeyCode::Enter => return ListEvent::Next,
            KeyCode::Right if !self.filter.has_selection() => {
                return self
                    .focused_id()
                    .map_or(ListEvent::Nothing, ListEvent::Open);
            }
            KeyCode::Left if self.filter.is_empty() => return ListEvent::Up,
            KeyCode::Backspace if self.filter.is_empty() => return ListEvent::Up,
            KeyCode::Esc if !self.filter.is_empty() => self.clear_search(),
            KeyCode::Esc => return ListEvent::Back,
            KeyCode::Char('a') if ctrl && self.filter.is_empty() => return ListEvent::ToggleAll,
            // With nothing typed yet, the keys that reach the rest of the interface (the command
            // line and help) still do; once a search is under way they are text like any other.
            KeyCode::Char(':' | '?') if self.filter.is_empty() => return ListEvent::Nothing,
            _ => {
                if !self.filter.handle_key(key) {
                    return ListEvent::Nothing;
                }
                self.cursor = 0;
                self.refilter();
            }
        }
        ListEvent::Moved
    }

    pub fn paste(&mut self, text: &str) {
        self.filter.paste(text);
        self.cursor = 0;
        self.refilter();
    }

    pub fn handle_mouse(&mut self, mouse: &MouseEvent) -> ListEvent {
        let at = Position::new(mouse.column, mouse.row);
        let search_column = usize::from(
            mouse
                .column
                .saturating_sub(self.search_area.x + SEARCH_PREFIX),
        );
        match mouse.kind {
            MouseEventKind::ScrollUp if self.list_area.contains(at) => self.move_by(-3),
            MouseEventKind::ScrollDown if self.list_area.contains(at) => self.move_by(3),
            MouseEventKind::Down(MouseButton::Left) if self.search_area.contains(at) => {
                let double = self
                    .last_click
                    .is_some_and(|(when, row)| row == usize::MAX && when.elapsed() < DOUBLE_CLICK);
                // The search line is remembered as row `usize::MAX`, apart from every list row.
                self.last_click = Some((Instant::now(), usize::MAX));
                if double {
                    self.filter.select_word_at(search_column);
                } else {
                    self.filter.click(search_column);
                }
            }
            MouseEventKind::Drag(MouseButton::Left) if self.search_area.contains(at) => {
                self.filter.drag_to(search_column);
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let Some(shown) = self
                    .rows
                    .iter()
                    .find(|(row, _)| *row == mouse.row && self.list_area.contains(at))
                    .map(|(_, shown)| *shown)
                else {
                    return ListEvent::Nothing;
                };
                self.cursor = shown;
                let double = self
                    .last_click
                    .is_some_and(|(when, row)| row == shown && when.elapsed() < DOUBLE_CLICK);
                self.last_click = Some((Instant::now(), shown));
                let Some(choice) = self.focused() else {
                    return ListEvent::Nothing;
                };
                return match (double, choice.pickable) {
                    (true, _) | (false, false) => ListEvent::Open(choice.id.clone()),
                    (false, true) => ListEvent::Toggle(choice.id.clone()),
                };
            }
            _ => return ListEvent::Nothing,
        }
        ListEvent::Moved
    }

    /// Draws the search line, a blank line and the rows. `picked` says which rows are picked.
    pub fn draw(&mut self, frame: &mut Frame, area: Rect, picked: &dyn Fn(&str) -> bool) {
        let [search_area, _, list_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .areas(area);
        self.search_area = search_area;
        self.list_area = list_area;

        let mut search = vec![Span::styled("\u{2315} ", theme::title())];
        if self.filter.is_empty() {
            search.push(Span::styled(self.placeholder, theme::muted().italic()));
        } else {
            search.extend(self.filter.spans(
                Style::default().fg(theme::LAVENDER),
                Style::default().fg(theme::INDIGO),
                true,
            ));
            search.push(Span::styled(
                format!("   {} found", self.shown.len()),
                theme::muted(),
            ));
        }
        frame.render_widget(Paragraph::new(Line::from(search)), search_area);

        // Section headings take a row each, so count them in when keeping the cursor in view.
        let lines_for = |from: usize, to: usize, list: &Self| -> usize {
            let mut lines = 0;
            let mut section = None;
            for shown in from..=to {
                let choice = list
                    .shown
                    .get(shown)
                    .and_then(|index| list.choices.get(*index));
                let this = choice.and_then(|choice| choice.section);
                if this.is_some() && (shown == from || this != section) {
                    lines += 1;
                }
                section = this;
                lines += 1;
            }
            lines
        };
        let height = usize::from(list_area.height).max(1);
        let mut top = 0;
        while top < self.cursor && lines_for(top, self.cursor, self) > height {
            top += 1;
        }

        let words = words(self.filter.value());
        let width = usize::from(list_area.width);
        let mut lines = Vec::new();
        self.rows.clear();
        let mut section = None;
        for (shown, index) in self.shown.iter().enumerate().skip(top) {
            let Some(choice) = self.choices.get(*index) else {
                continue;
            };
            if let Some(heading) = choice.section
                && (shown == top || choice.section != section)
            {
                lines.push(Line::styled(
                    format!("  {}", heading.to_uppercase()),
                    Style::default()
                        .fg(theme::INDIGO_LIGHT)
                        .add_modifier(Modifier::BOLD),
                ));
            }
            section = choice.section;
            if lines.len() >= height {
                break;
            }
            let y = list_area.y + u16::try_from(lines.len()).unwrap_or(u16::MAX);
            self.rows.push((y, shown));
            let focused = shown == self.cursor;
            let mut line = row_line(
                choice,
                focused,
                picked(&choice.id),
                self.picks,
                &words,
                width,
            );
            if focused {
                line = line.style(theme::selection());
            }
            lines.push(line);
            if lines.len() >= height {
                break;
            }
        }
        if self.shown.is_empty() {
            lines.push(Line::styled(
                if self.filter.is_empty() {
                    "  nothing here"
                } else {
                    "  nothing matches - esc clears the search"
                },
                theme::muted().italic(),
            ));
        }
        frame.render_widget(Paragraph::new(lines), list_area);
    }
}

fn words(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_lowercase).collect()
}

/// One row: cursor marker, pick column, mark, title with the search matches lit, detail, and the
/// note at the right edge.
fn row_line(
    choice: &Choice,
    focused: bool,
    picked: bool,
    picks: Picks,
    words: &[String],
    width: usize,
) -> Line<'static> {
    let mut spans = vec![Span::styled(
        if focused { "\u{203a} " } else { "  " },
        theme::title(),
    )];
    spans.push(if choice.pickable {
        pick_mark(picks, picked)
    } else {
        Span::raw("    ")
    });
    if let Some(mark) = &choice.mark {
        spans.push(mark.clone());
        spans.push(Span::raw(" "));
    }

    let note_width: usize = choice.note.iter().map(Span::width).sum();
    let used: usize = spans.iter().map(Span::width).sum();
    let room = width.saturating_sub(used + note_width + 2);

    let title_color = if picked {
        theme::MINT
    } else {
        choice.tone.unwrap_or(theme::LAVENDER)
    };
    let title = ellipsize(&choice.title, room);
    let title_width = title.chars().count();
    spans.extend(lit(
        &title,
        words,
        Style::default()
            .fg(title_color)
            .add_modifier(Modifier::BOLD),
    ));
    let detail_room = room.saturating_sub(title_width + 2);
    let mut text_width = title_width;
    if !choice.detail.is_empty() && detail_room > 3 {
        let detail = ellipsize(&choice.detail, detail_room);
        text_width += 2 + detail.chars().count();
        spans.push(Span::raw("  "));
        spans.extend(lit(&detail, words, theme::muted()));
    }
    if !choice.note.is_empty() {
        spans.push(Span::raw(" ".repeat(room.saturating_sub(text_width) + 2)));
        spans.extend(choice.note.iter().cloned());
    }
    Line::from(spans)
}

/// The pick column of a row: `[✓]` / `[ ]` where several can be picked, `(●)` / `( )` where one
/// is. Other controls that pick (such as a form's choice between a few options) draw it too, so
/// picking looks the same everywhere.
pub fn pick_mark(picks: Picks, picked: bool) -> Span<'static> {
    let mark = match (picks, picked) {
        (Picks::Many, true) => "[\u{2713}] ",
        (Picks::Many, false) => "[ ] ",
        (Picks::One, true) => "(\u{25cf}) ",
        (Picks::One, false) => "( ) ",
    };
    let style = if picked {
        Style::default()
            .fg(theme::MINT)
            .add_modifier(Modifier::BOLD)
    } else {
        theme::muted()
    };
    Span::styled(mark, style)
}

/// `text` in `style`, with every place a search word matches drawn in amber and underlined.
fn lit(text: &str, words: &[String], style: Style) -> Vec<Span<'static>> {
    let lower = text.to_lowercase();
    let chars: Vec<char> = text.chars().collect();
    let lower_chars: Vec<char> = lower.chars().collect();
    // Lowercasing can change the length of some characters; match only when it does not.
    if chars.len() != lower_chars.len() || words.is_empty() {
        return vec![Span::styled(text.to_owned(), style)];
    }
    let mut lit = vec![false; chars.len()];
    for word in words {
        let word: Vec<char> = word.chars().collect();
        if word.is_empty() {
            continue;
        }
        let mut at = 0;
        while at + word.len() <= lower_chars.len() {
            if lower_chars[at..at + word.len()] == word[..] {
                lit[at..at + word.len()]
                    .iter_mut()
                    .for_each(|on| *on = true);
                at += word.len();
            } else {
                at += 1;
            }
        }
    }
    let hit = style.fg(theme::AMBER).add_modifier(Modifier::UNDERLINED);
    let mut spans = Vec::new();
    let mut run = String::new();
    let mut run_lit = false;
    for (char, on) in chars.into_iter().zip(lit) {
        if on != run_lit && !run.is_empty() {
            spans.push(Span::styled(
                std::mem::take(&mut run),
                if run_lit { hit } else { style },
            ));
        }
        run_lit = on;
        run.push(char);
    }
    if !run.is_empty() {
        spans.push(Span::styled(run, if run_lit { hit } else { style }));
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choice(id: &str, title: &str, keywords: &str) -> Choice {
        Choice {
            id: id.to_owned(),
            title: title.to_owned(),
            keywords: keywords.to_owned(),
            pickable: true,
            ..Choice::default()
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn search_matches_keywords_and_puts_title_matches_first() {
        let mut list = SelectList::new(Picks::One, "search");
        list.set_choices(vec![
            choice("auto", "Only my requests", "split database postgres"),
            choice("db", "Branch databases", ""),
        ]);
        for char in "data".chars() {
            list.handle_key(&key(KeyCode::Char(char)));
        }
        assert_eq!(list.shown_ids(), vec!["db".to_owned(), "auto".to_owned()]);

        list.handle_key(&key(KeyCode::Char('x')));
        assert!(list.shown_ids().is_empty());
        list.handle_key(&key(KeyCode::Esc));
        assert_eq!(list.shown_ids().len(), 2);
        assert_eq!(list.handle_key(&key(KeyCode::Esc)), ListEvent::Back);
    }

    #[test]
    fn space_toggles_enter_goes_on_and_ctrl_a_picks_all() {
        let mut list = SelectList::new(Picks::Many, "search");
        list.set_choices(vec![choice("a", "a", ""), choice("b", "b", "")]);
        list.handle_key(&key(KeyCode::Down));
        assert_eq!(
            list.handle_key(&key(KeyCode::Char(' '))),
            ListEvent::Toggle("b".to_owned())
        );
        assert_eq!(list.handle_key(&key(KeyCode::Enter)), ListEvent::Next);
        assert_eq!(
            list.handle_key(&KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL)),
            ListEvent::ToggleAll
        );
    }

    #[test]
    fn matches_light_up_inside_the_title() {
        let spans = lit("checkout-api", &["out".to_owned()], Style::default());
        let texts: Vec<&str> = spans.iter().map(|span| span.content.as_ref()).collect();
        assert_eq!(texts, vec!["check", "out", "-api"]);
    }
}
