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

#[cfg(test)]
mod tests {
    use super::ellipsize;

    #[test]
    fn ellipsize_clamps_and_marks_cuts() {
        assert_eq!(ellipsize("short", 10), "short");
        assert_eq!(ellipsize("exactly-ten", 11), "exactly-ten");
        assert_eq!(ellipsize("a-very-long-deployment-name", 10), "a-very-lo…");
        assert_eq!(ellipsize("anything", 0), "");
        assert_eq!(ellipsize("ab", 1), "…");
    }
}
