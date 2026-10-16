//! How the connection is going, at the bottom of the sidebar, and the dialog holding what does not
//! fit there.

use kube::Client;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Clear, Paragraph},
};

use crate::{
    helpers::{centered, ellipsize},
    scope::Scope,
    screens::targets::history,
    stderr, theme,
};

/// How much of the full error the details dialog shows.
///
/// `kube` builds its auth errors out of the whole failed command - every environment variable it
/// was handed, the `KUBERNETES_EXEC_INFO` JSON included - followed by a debug dump of the plugin's
/// output. Several hundred characters in, it stops telling the reader anything new, and the log
/// keeps the untruncated version anyway.
const DETAIL_LIMIT: usize = 400;

/// Widest the details dialog is allowed to get, so the text still reads as paragraphs on a wide
/// terminal instead of stretching across it.
const DETAIL_WIDTH: u16 = 90;

/// Most lines a failed connection's summary takes in the sidebar; the `e` dialog has the rest.
const SUMMARY_LINES: usize = 3;

/// The bottom of the sidebar, `width` columns wide: the cluster, the namespace, and how the
/// connection is going. A failed connection gets its summary, cut to a few lines, and the key
/// that shows the whole of it.
///
/// Folded to a few columns, only the connection's sign is left.
pub fn sidebar_footer(
    scope: &Scope,
    client: &Option<anyhow::Result<Client>>,
    width: u16,
) -> Vec<Line<'static>> {
    let width = usize::from(width);
    let state = match client {
        Some(Ok(_)) => Span::styled("\u{25cf} connected", Style::default().fg(theme::MINT)),
        None => Span::styled("\u{25cc} connecting\u{2026}", theme::warning()),
        Some(Err(_)) => Span::styled(
            "\u{2717} can't connect",
            theme::error().add_modifier(Modifier::BOLD),
        ),
    };
    if width < 8 {
        let sign: String = state.content.chars().take(1).collect();
        return vec![Line::from(Span::styled(format!(" {sign}"), state.style))];
    }

    let value = Style::default().fg(theme::LAVENDER);
    let mut lines = vec![
        // With no context picked, the kubeconfig's current one is in use: name it.
        Line::styled(
            format!(
                " {}",
                ellipsize(
                    &scope.context.clone().unwrap_or_else(history::cluster),
                    width - 1
                )
            ),
            value,
        ),
        // Lists read every namespace until one is picked.
        match scope.namespace.as_deref() {
            Some(name) => Line::styled(format!(" {}", ellipsize(name, width - 1)), value),
            None => Line::styled(" all namespaces", theme::muted().italic()),
        },
        Line::from(vec![Span::raw(" "), state]),
    ];
    if let Some(Err(error)) = client {
        let mut summary = wrap(&headline(error), width - 1);
        if summary.len() > SUMMARY_LINES {
            summary.truncate(SUMMARY_LINES);
            if let Some(last) = summary.last_mut() {
                *last = ellipsize(&format!("{last}\u{2026}"), width - 1);
            }
        }
        lines.extend(
            summary
                .into_iter()
                .map(|line| Line::styled(format!(" {line}"), theme::error())),
        );
        lines.push(Line::from(vec![
            Span::styled(" <e>", theme::title()),
            Span::styled(" details", theme::muted()),
        ]));
    }
    lines
}

/// Draws the `e` connection error dialog over `area`.
pub fn draw_details(frame: &mut Frame, area: Rect, error: &anyhow::Error) {
    let width = DETAIL_WIDTH.min(area.width.saturating_sub(4));
    let inner_width = width.saturating_sub(2) as usize;

    // Wrapped here rather than by the paragraph, so the dialog is exactly as tall as its contents
    // instead of as tall as an estimate of them - guessing high leaves a band of empty rows under
    // the text, and guessing low cuts the last line off against the bottom border.
    let body = detail_lines(error, inner_width);
    let footer = Line::from(Span::styled("esc to close", theme::muted()));

    // The body, a blank row, the footer, and the two borders.
    let height = (body.len() as u16 + 4).min(area.height.saturating_sub(2));

    let dialog = centered(
        area,
        Constraint::Length(width),
        Constraint::Length(height.max(3)),
    );

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme::error())
        .title(Span::styled(
            " connection error ",
            theme::error().add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(dialog);

    frame.render_widget(Clear, dialog);
    frame.render_widget(block, dialog);

    // The footer keeps its row even when the body has to be clipped to fit a short terminal: a
    // truncated dump is survivable, a dialog with no way out written in it is not.
    let [body_area, footer_area] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(inner);

    frame.render_widget(Paragraph::new(body), body_area);
    frame.render_widget(Paragraph::new(footer), footer_area);
}

/// The dialog's contents, wrapped to `width`: the summary the status bar had to cut short, then
/// what the credential plugin printed, then as much of the error itself as is still worth reading.
///
/// The plugin's own words come first because they are the ones that say what to do about the
/// failure - the error wrapped around them only says which command produced it.
fn detail_lines(error: &anyhow::Error, width: usize) -> Vec<Line<'static>> {
    let mut lines = styled(
        &headline(error),
        width,
        theme::error().add_modifier(Modifier::BOLD),
    );

    let plain = Style::default().fg(theme::LAVENDER);

    let captured = stderr::recent();
    if !captured.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled("captured output", theme::muted())));
        lines.extend(captured.iter().flat_map(|line| styled(line, width, plain)));
    }

    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled("details", theme::muted())));
    lines.extend(styled(
        &ellipsize(&one_line(&format!("{error:#}")), DETAIL_LIMIT),
        width,
        plain,
    ));

    lines
}

/// `text` wrapped to `width` and styled, one [`Line`] per rendered row.
fn styled(text: &str, width: usize, style: Style) -> Vec<Line<'static>> {
    wrap(text, width)
        .into_iter()
        .map(|row| Line::from(Span::styled(row, style)))
        .collect()
}

/// Breaks `text` into rows of at most `width` characters, at spaces where it can.
///
/// A word longer than the whole row is split rather than left to overflow: the payload `kube`
/// quotes into an auth error is one unbroken run of JSON hundreds of characters long, so this is
/// the common case here rather than the pathological one.
fn wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }

    let mut rows: Vec<String> = Vec::new();
    let mut row = String::new();
    let mut taken = 0;

    for word in text.split_whitespace() {
        for chunk in chunks(word, width) {
            let length = chunk.chars().count();
            let separated = usize::from(taken > 0);

            if taken + separated + length > width {
                rows.push(std::mem::take(&mut row));
                taken = 0;
            } else if separated > 0 {
                row.push(' ');
                taken += 1;
            }

            row.push_str(&chunk);
            taken += length;
        }
    }

    if !row.is_empty() || rows.is_empty() {
        rows.push(row);
    }

    rows
}

/// `word` in pieces of at most `width` characters, which is `word` itself unless it is too long to
/// ever fit a row.
fn chunks(word: &str, width: usize) -> Vec<String> {
    if word.chars().count() <= width {
        return vec![word.to_owned()];
    }

    let mut pieces = Vec::new();
    let mut piece = String::new();

    for character in word.chars() {
        if piece.chars().count() == width {
            pieces.push(std::mem::take(&mut piece));
        }
        piece.push(character);
    }
    if !piece.is_empty() {
        pieces.push(piece);
    }

    pieces
}

/// Condenses a connection failure into something that fits on one line.
///
/// Errors built around a payload get cut back to the classification in front of it: `kube`'s auth
/// errors quote the entire failed command, `KUBERNETES_EXEC_INFO`'s JSON and all, so
/// `cluster unreachable: auth error: auth exec command '<400 characters>' failed` becomes
/// `cluster unreachable: auth error: auth exec command`. What was cut is in the details dialog.
fn headline(error: &anyhow::Error) -> String {
    let full = one_line(&format!("{error:#}"));

    let payload = full.find(['\'', '"', '{', '[']).unwrap_or(full.len());
    let summary = full[..payload].trim_end_matches([' ', ':', '-', ',']);

    // A message that opens with its payload has no classification to keep; a stub of one would say
    // less than the whole thing does.
    match summary.chars().count() >= MIN_HEADLINE {
        true => summary.to_owned(),
        false => full,
    }
}

/// Shortest prefix that still reads as a description of what went wrong rather than as a fragment.
const MIN_HEADLINE: usize = 8;

/// Flattens `text` onto one line, collapsing every run of whitespace into a single space.
///
/// Error messages carry newlines and indentation from whatever produced them, and a `Span` holding
/// either renders as broken cells rather than as the wrapped text it looks like in a log file.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::{headline, one_line, wrap};

    /// The real thing, as `kube` renders it when `gke-gcloud-auth-plugin` cannot refresh a token:
    /// the failed `Command`'s debug output, which is the environment it was given followed by the
    /// binary, and then a debug dump of what it wrote.
    #[test]
    fn headline_drops_the_payload_an_auth_error_is_built_around() {
        let error = anyhow::anyhow!(
            "cluster unreachable: auth error: auth exec command \
             'KUBERNETES_EXEC_INFO=\"{{\\\"kind\\\":\\\"ExecCredential\\\"}}\" \
             \"gke-gcloud-auth-plugin\"' failed with status exit status 1: \
             Output {{ status: ExitStatus(1), stdout: \"\", stderr: \"\" }}"
        );

        assert_eq!(
            headline(&error),
            "cluster unreachable: auth error: auth exec command"
        );
    }

    #[test]
    fn headline_keeps_messages_that_have_no_payload_whole() {
        let error = anyhow::anyhow!("cluster unreachable: connection refused");

        assert_eq!(headline(&error), "cluster unreachable: connection refused");
    }

    #[test]
    fn headline_keeps_the_whole_message_rather_than_a_stub_of_it() {
        // Cutting at the quote would leave "no" behind, which says less than the line does.
        let error = anyhow::anyhow!("no \"kubeconfig\" file was found");

        assert_eq!(headline(&error), "no \"kubeconfig\" file was found");
    }

    #[test]
    fn headline_flattens_a_multi_line_message() {
        let error =
            anyhow::anyhow!("reauthentication failed.\n\n  Please run:\n\tgcloud auth login");

        assert_eq!(
            headline(&error),
            "reauthentication failed. Please run: gcloud auth login"
        );
    }

    #[test]
    fn wrap_breaks_at_spaces() {
        assert_eq!(wrap("one two three four", 9), ["one two", "three", "four"]);
        assert_eq!(wrap("fits exactly", 12), ["fits exactly"]);
    }

    #[test]
    fn wrap_splits_a_word_too_long_for_a_row() {
        // The quoted `KUBERNETES_EXEC_INFO` payload is one unbroken run like this; left whole it
        // would overflow the dialog rather than wrap inside it.
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap("hi abcdefghij", 4), ["hi", "abcd", "efgh", "ij"]);
    }

    #[test]
    fn wrap_always_yields_a_row_and_never_an_impossible_one() {
        assert_eq!(wrap("", 10), [""]);
        assert!(wrap("anything", 0).is_empty());
        assert!(
            wrap("a b c d e", 3)
                .iter()
                .all(|row| row.chars().count() <= 3)
        );
    }

    #[test]
    fn one_line_collapses_every_kind_of_whitespace() {
        assert_eq!(one_line("  a\n\tb   c \r\n"), "a b c");
        assert_eq!(one_line(""), "");
    }
}
