//! Draws the markdown that mirrord's docs are written in, as styled terminal lines.
//!
//! Covers what the config docs use: headings, paragraphs, `-`, `*` and numbered lists, `>`
//! quotes, fenced code blocks, and inline `**bold**`, `*italic*`, `` `code` `` and
//! `[links](url)`. Anything else is shown as the text it is. Paragraphs are re-flowed to the
//! width they are drawn at, since the docs hard-wrap their source at 100 columns.

use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

use crate::{helpers::ellipsize, theme};

/// Marks where a fenced code block starts and ends.
const FENCE: &str = "```";

/// One styled run of text.
type Piece = (String, Style);

/// A word: the pieces between two spaces, which may change style midway (`` `steal`, ``).
type Word = Vec<Piece>;

/// The kinds of block the renderer tells apart.
enum Block {
    Blank,
    Heading(String),
    Code(String),
    /// A list item: its indent, its marker (`• `, `2. `) and its text.
    Item {
        indent: usize,
        marker: String,
        text: String,
    },
    Quote(String),
    Paragraph(String),
}

/// Renders `text` into lines at most `width` columns wide.
pub fn render(text: &str, width: usize) -> Vec<Line<'static>> {
    let width = width.max(8);
    let mut lines: Vec<Line<'static>> = Vec::new();

    for block in blocks(text) {
        match block {
            Block::Blank => {
                if lines.last().is_some_and(|line| line.width() > 0) {
                    lines.push(Line::raw(""));
                }
            }
            Block::Heading(text) => {
                if lines.last().is_some_and(|line| line.width() > 0) {
                    lines.push(Line::raw(""));
                }
                let style = Style::default()
                    .fg(theme::LAVENDER)
                    .add_modifier(Modifier::BOLD);
                lines.extend(wrap(words(&text, style), width, "", ""));
            }
            Block::Code(code) => lines.push(Line::styled(
                ellipsize(&format!("  {code}"), width),
                Style::default().fg(theme::INDIGO_LIGHT),
            )),
            Block::Item {
                indent,
                marker,
                text,
            } => {
                let first = format!("{}{marker}", " ".repeat(indent));
                let rest = " ".repeat(first.chars().count());
                lines.extend(wrap(words(&text, Style::default()), width, &first, &rest));
            }
            Block::Quote(text) => {
                lines.extend(wrap(words(&text, theme::muted()), width, "│ ", "│ "));
            }
            Block::Paragraph(text) => {
                lines.extend(wrap(words(&text, Style::default()), width, "", ""));
            }
        }
    }

    while lines.last().is_some_and(|line| line.width() == 0) {
        lines.pop();
    }
    lines
}

/// Splits `text` into blocks, joining the lines of a paragraph or a list item back together.
fn blocks(text: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut in_code = false;

    for raw in text.lines() {
        if raw.trim_start().starts_with(FENCE) {
            in_code = !in_code;
            continue;
        }
        if in_code {
            blocks.push(Block::Code(raw.to_owned()));
            continue;
        }

        let line = strip_comments(raw);
        let trimmed = line.trim();
        let indent = line.len() - line.trim_start().len();

        if trimmed.is_empty() {
            // A line that only held an HTML comment is not a paragraph break.
            if raw.trim().is_empty() {
                blocks.push(Block::Blank);
            }
            continue;
        }

        if let Some(heading) = trimmed.strip_prefix('#') {
            let heading = heading.trim_start_matches('#').trim();
            // Headings carry their docs anchor, `{#feature-network}`, which is no use here.
            let heading = heading.split(" {#").next().unwrap_or(heading);
            blocks.push(Block::Heading(heading.to_owned()));
            continue;
        }
        if let Some(quote) = trimmed.strip_prefix('>') {
            blocks.push(Block::Quote(quote.trim().to_owned()));
            continue;
        }
        if let Some((marker, text)) = list_marker(trimmed) {
            blocks.push(Block::Item {
                indent,
                marker,
                text: text.to_owned(),
            });
            continue;
        }

        // A plain line continues the paragraph or list item right above it.
        match blocks.last_mut() {
            Some(Block::Paragraph(text) | Block::Item { text, .. }) => {
                text.push(' ');
                text.push_str(trimmed);
            }
            _ => blocks.push(Block::Paragraph(trimmed.to_owned())),
        }
    }

    blocks
}

/// The marker a list item starts with, as drawn, and the item's text.
fn list_marker(line: &str) -> Option<(String, &str)> {
    if let Some(text) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
        return Some(("• ".to_owned(), text));
    }
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    let text = line.get(digits..)?.strip_prefix(". ")?;
    (digits > 0).then(|| {
        (
            format!("{}. ", line.get(..digits).unwrap_or_default()),
            text,
        )
    })
}

/// Removes `<!-- ... -->` comments, which the docs use for notes to the docs generator.
fn strip_comments(line: &str) -> String {
    let mut rest = line;
    let mut kept = String::new();
    while let Some(start) = rest.find("<!--") {
        kept.push_str(rest.get(..start).unwrap_or_default());
        match rest.get(start..).and_then(|tail| tail.find("-->")) {
            Some(end) => rest = rest.get(start + end + 3..).unwrap_or_default(),
            None => {
                rest = "";
                break;
            }
        }
    }
    kept.push_str(rest);
    kept
}

/// Splits `text` into words, each made of the styled pieces its inline markup asks for.
fn words(text: &str, base: Style) -> Vec<Word> {
    let mut words: Vec<Word> = Vec::new();
    let mut word: Word = Vec::new();

    for (piece, style) in inline(text, base) {
        let mut parts = piece.split(' ').peekable();
        while let Some(part) = parts.next() {
            if !part.is_empty() {
                word.push((part.to_owned(), style));
            }
            // Every space ends a word; text after the last space starts the next one.
            if parts.peek().is_some() && !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

/// Splits `text` into styled pieces by its inline markup.
fn inline(text: &str, base: Style) -> Vec<Piece> {
    let mut pieces: Vec<Piece> = Vec::new();
    let mut current = String::new();
    let mut bold = false;
    let mut italic = false;

    let style = |bold: bool, italic: bool| {
        let mut style = base;
        if bold {
            style = style.add_modifier(Modifier::BOLD);
        }
        if italic {
            style = style.add_modifier(Modifier::ITALIC);
        }
        style
    };

    let chars: Vec<char> = text.chars().collect();
    let mut index = 0;
    while let Some(&char) = chars.get(index) {
        let next = chars.get(index + 1).copied();
        match char {
            '\\' if next.is_some() => {
                current.extend(next);
                index += 2;
                continue;
            }
            '`' => {
                let close = chars
                    .iter()
                    .skip(index + 1)
                    .position(|candidate| *candidate == '`');
                if let Some(length) = close {
                    flush(&mut pieces, &mut current, style(bold, italic));
                    let code: String = chars.iter().skip(index + 1).take(length).collect();
                    pieces.push((code, base.fg(theme::INDIGO_LIGHT)));
                    index += length + 2;
                    continue;
                }
            }
            '*' if next == Some('*') => {
                flush(&mut pieces, &mut current, style(bold, italic));
                bold = !bold;
                index += 2;
                continue;
            }
            // An asterisk only opens emphasis right before a word, so `a * b` stays as it is.
            '*' if italic || next.is_some_and(|next| !next.is_whitespace()) => {
                flush(&mut pieces, &mut current, style(bold, italic));
                italic = !italic;
                index += 1;
                continue;
            }
            '[' => {
                if let Some((label, consumed)) = link(&chars, index) {
                    flush(&mut pieces, &mut current, style(bold, italic));
                    pieces.push((
                        label,
                        style(bold, italic).add_modifier(Modifier::UNDERLINED),
                    ));
                    index += consumed;
                    continue;
                }
            }
            _ => {}
        }
        current.push(char);
        index += 1;
    }
    flush(&mut pieces, &mut current, style(bold, italic));
    pieces
}

/// The label of a `[label](url)` link starting at `start`, and how many characters it spans.
/// The URL is left out: it cannot be followed from the terminal, and the label says what it is.
fn link(chars: &[char], start: usize) -> Option<(String, usize)> {
    let close_label = chars.iter().skip(start + 1).position(|char| *char == ']')? + start + 1;
    if chars.get(close_label + 1) != Some(&'(') {
        return None;
    }
    let close_url = chars
        .iter()
        .skip(close_label + 2)
        .position(|char| *char == ')')?
        + close_label
        + 2;
    // Labels are often code (`[`steal`](#steal)`); the underline already marks them out.
    let label: String = chars
        .iter()
        .skip(start + 1)
        .take(close_label - start - 1)
        .filter(|char| **char != '`')
        .collect();
    Some((label, close_url - start + 1))
}

fn flush(pieces: &mut Vec<Piece>, current: &mut String, style: Style) {
    if !current.is_empty() {
        pieces.push((std::mem::take(current), style));
    }
}

/// Lays `words` out in lines at most `width` wide, the first starting with `first` and the rest
/// with `rest`. A word too long for a line of its own is split across lines.
fn wrap(words: Vec<Word>, width: usize, first: &str, rest: &str) -> Vec<Line<'static>> {
    let word_width =
        |word: &Word| -> usize { word.iter().map(|(text, _)| text.chars().count()).sum() };

    let mut lines = Vec::new();
    let mut line: Vec<Span<'static>> = vec![Span::styled(first.to_owned(), theme::muted())];
    let mut used = first.chars().count();
    let mut empty = true;

    for word in words {
        let length = word_width(&word);
        if !empty && used + 1 + length > width {
            lines.push(Line::from(std::mem::take(&mut line)));
            line.push(Span::styled(rest.to_owned(), theme::muted()));
            used = rest.chars().count();
            empty = true;
        }
        if !empty {
            line.push(Span::raw(" "));
            used += 1;
        }

        for (text, style) in word {
            let mut text = text.as_str();
            while used + text.chars().count() > width && used < width && !text.is_empty() {
                let room = width - used;
                let split = text
                    .char_indices()
                    .nth(room)
                    .map_or(text.len(), |(index, _)| index);
                line.push(Span::styled(
                    text.get(..split).unwrap_or_default().to_owned(),
                    style,
                ));
                lines.push(Line::from(std::mem::take(&mut line)));
                line.push(Span::styled(rest.to_owned(), theme::muted()));
                used = rest.chars().count();
                text = text.get(split..).unwrap_or_default();
            }
            used += text.chars().count();
            line.push(Span::styled(text.to_owned(), style));
        }
        empty = false;
    }

    if !empty || lines.is_empty() {
        lines.push(Line::from(line));
    }
    lines
}

#[cfg(test)]
mod tests {
    use ratatui::style::Modifier;

    use super::render;
    use crate::theme;

    fn text(lines: &[ratatui::text::Line]) -> Vec<String> {
        lines.iter().map(|line| line.to_string()).collect()
    }

    #[test]
    fn paragraphs_reflow_and_markup_becomes_style() {
        let lines = render(
            "Incoming traffic supports\n2 modes:\n\n1. Mirror (**default**): sniffs `TCP` data,\n   forwarding a copy;\n2. Steal",
            40,
        );
        assert_eq!(
            text(&lines),
            [
                "Incoming traffic supports 2 modes:",
                "",
                "1. Mirror (default): sniffs TCP data,",
                "   forwarding a copy;",
                "2. Steal",
            ]
        );

        let item = lines.get(2).expect("the first list item is drawn");
        let bold = item
            .spans
            .iter()
            .find(|span| span.content.contains("default"))
            .expect("the bold word is drawn");
        assert!(bold.style.add_modifier.contains(Modifier::BOLD));
        let code = item
            .spans
            .iter()
            .find(|span| span.content == "TCP")
            .expect("the code word is drawn");
        assert_eq!(code.style.fg, Some(theme::INDIGO_LIGHT));
    }

    #[test]
    fn code_blocks_headings_links_and_comments() {
        let lines = render(
            "<!--${internal}-->\n### Minimal config {#feature-network}\n\nSee the [reference](https://metalbear.com/x) page, and [`steal`](##steal).\n\n```json\n{\n  \"incoming\": \"steal\"\n}\n```",
            60,
        );
        assert_eq!(
            text(&lines),
            [
                "Minimal config",
                "",
                "See the reference page, and steal.",
                "",
                "  {",
                "    \"incoming\": \"steal\"",
                "  }",
            ]
        );
    }

    #[test]
    fn long_words_are_split_rather_than_overflowing() {
        let lines = render("- aaaaaaaaaaaaaaaaaaaa", 12);
        assert!(
            lines.iter().all(|line| line.width() <= 12),
            "{:?}",
            text(&lines)
        );
        assert_eq!(
            text(&lines).concat().replace([' ', '•'], ""),
            "aaaaaaaaaaaaaaaaaaaa"
        );
    }
}
