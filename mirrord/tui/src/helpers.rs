use ratatui::{
    Frame,
    layout::{Constraint, Flex, Layout, Rect},
    style::{Modifier, Style},
    text::Span,
    widgets::{Block, BorderType, Clear},
};

use crate::theme;

/// Returns a rectangle centred in `area`.
pub fn centered(area: Rect, width: Constraint, height: Constraint) -> Rect {
    let [result] = Layout::vertical([height]).flex(Flex::Center).areas(area);
    let [result] = Layout::horizontal([width]).flex(Flex::Center).areas(result);

    result
}

/// Draws a dialog over `area`.
///
/// Returns the rectangle to draw contents into.
#[allow(unused, reason = "Nothing uses this yet.")]
pub fn dialog(frame: &mut Frame, area: Rect, title: &str) -> Rect {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::INDIGO))
        .title(Span::styled(
            format!(" {} ", title.trim()),
            Style::default()
                .fg(theme::LAVENDER)
                .bg(theme::DEEP)
                .add_modifier(Modifier::BOLD),
        ));

    let inner = block.inner(area);

    frame.render_widget(Clear, area);
    frame.render_widget(block, area);

    inner
}

/// Clamps `text` to `max` characters, ending in `…` when it had to cut.
/// Panels have limited widths and names can be arbitrarily long, and a value
/// silently cut off by a widget reads like the whole value.
pub fn ellipsize(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    match max {
        0 => String::new(),
        _ => {
            let mut clipped: String = text.chars().take(max - 1).collect();
            clipped.push('…');
            clipped
        }
    }
}

/// Splits `text` into lines at most `width` characters long, breaking between words where it can.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut line = String::new();

    for word in text.split_whitespace() {
        let mut word = word;

        loop {
            let line_len = line.chars().count();
            let separator = usize::from(line_len > 0);

            if line_len + separator + word.chars().count() <= width {
                if separator > 0 {
                    line.push(' ');
                }
                line.push_str(word);

                break;
            }

            if line_len > 0 {
                lines.push(std::mem::take(&mut line));

                continue;
            }

            let split_at = word
                .char_indices()
                .nth(width)
                .map_or(word.len(), |(index, _)| index);
            lines.push(word[..split_at].to_owned());
            word = &word[split_at..];

            if word.is_empty() {
                break;
            }
        }
    }

    if !line.is_empty() {
        lines.push(line);
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::{ellipsize, wrap};

    #[test]
    fn ellipsize_clamps_and_marks_cuts() {
        assert_eq!(ellipsize("short", 10), "short");
        assert_eq!(ellipsize("exactly-ten", 11), "exactly-ten");
        assert_eq!(ellipsize("a-very-long-deployment-name", 10), "a-very-lo…");
        assert_eq!(ellipsize("anything", 0), "");
        assert_eq!(ellipsize("ab", 1), "…");
    }

    #[test]
    fn wrap_breaks_between_words_and_splits_long_ones() {
        assert_eq!(
            wrap("container `app` is waiting: ContainerCreating", 20),
            ["container `app` is", "waiting:", "ContainerCreating"]
        );
        assert_eq!(wrap("abcdefgh", 3), ["abc", "def", "gh"]);
        assert!(wrap("", 10).is_empty());
    }
}
