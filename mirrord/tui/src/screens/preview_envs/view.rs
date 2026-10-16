//! Flattening the grouped tree into visible rows, and drawing them as one dense table in the
//! k9s style: namespace and target rows as foldable group headers, each preview environment a
//! row with its phase, status, traffic mode and image.

use std::sync::Arc;

use kube::ResourceExt;
use mirrord_operator::crd::preview::{PreviewSession, PreviewSessionPhase};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Clear, Paragraph, Wrap},
};

use super::{
    data::PreviewEnvsTree,
    ui::{Mode, Selection, StopCandidate, TargetKey, UiState},
};
use crate::{
    helpers::{centered, ellipsize},
    kit::{
        panel, state_view,
        tree::{Fold, TreeRow},
    },
    theme,
};

/// Colors this screen gives a meaning of its own. Shared brand colors come from [`crate::theme`],
/// which also swaps all of them for named colors on terminals without 24-bit color.
mod brand {
    use ratatui::style::Color;

    pub use crate::theme::{
        AMBER as YELLOW, INDIGO as PURPLE, INK, MINT as SUCCESS, MUTED as GREY, SKY as NAMESPACE,
        TEAL as TARGET,
    };

    /// `Initializing`: `Waiting`'s purple, dimmed.
    pub const PURPLE_DIM: Color = Color::Rgb(0x7A, 0x78, 0xA0);
    /// `Failed`, a traffic-light red that reads as stronger than the interface's own error coral.
    pub const RED: Color = Color::Rgb(0xFF, 0x5F, 0x57);

    /// `Paused`: `Idle`'s yellow, dimmed exactly as [`PURPLE_DIM`] dims [`PURPLE`]. `Idle` and
    /// `Paused` are the two scaled-to-zero phases and read as a pair for that reason; only `Idle`
    /// still wakes on traffic, so the paused one is the quieter of the two.
    pub const YELLOW_DIM: Color = Color::Rgb(0xB8, 0x96, 0x5E);
}

fn phase_color(session: &PreviewSession) -> Color {
    match session.status.as_ref().map(|status| status.phase) {
        Some(PreviewSessionPhase::Ready) => brand::SUCCESS,
        Some(PreviewSessionPhase::Idle) => brand::YELLOW,
        Some(PreviewSessionPhase::Paused) => brand::YELLOW_DIM,
        Some(PreviewSessionPhase::Waiting) => brand::PURPLE,
        Some(PreviewSessionPhase::Initializing) => brand::PURPLE_DIM,
        Some(PreviewSessionPhase::Failed) => brand::RED,
        Some(PreviewSessionPhase::Unknown) | None => brand::GREY,
    }
}

/// One visible row: a namespace header, a target header, or a leaf env card.
#[derive(Clone)]
pub enum Row {
    NamespaceHeader {
        namespace: String,
        collapsed: bool,
        env_count: usize,
    },
    TargetHeader {
        namespace: String,
        target: TargetKey,
        collapsed: bool,
        env_count: usize,
    },
    Env {
        namespace: String,
        target: TargetKey,
        session: Arc<PreviewSession>,
    },
}

impl TreeRow for Row {
    type Id = Selection;

    fn id(&self) -> Selection {
        self.selection()
    }

    fn fold(&self) -> Fold {
        match self {
            Row::NamespaceHeader { collapsed, .. } | Row::TargetHeader { collapsed, .. } => {
                if *collapsed {
                    Fold::Collapsed
                } else {
                    Fold::Expanded
                }
            }
            Row::Env { .. } => Fold::Leaf,
        }
    }
}

impl Row {
    pub fn selection(&self) -> Selection {
        match self {
            Row::NamespaceHeader { namespace, .. } => Selection::Namespace(namespace.clone()),
            Row::TargetHeader {
                namespace, target, ..
            } => Selection::Target(namespace.clone(), target.clone()),
            Row::Env {
                namespace,
                target,
                session,
            } => Selection::Env(namespace.clone(), target.clone(), session.name_any()),
        }
    }
}

/// Turns the grouped tree into an ordered list of visible rows under the current filter and
/// collapse state. A target/namespace group with zero surviving children after filtering is
/// omitted entirely, not shown empty.
pub fn flatten(tree: &PreviewEnvsTree, ui: &UiState) -> Vec<Row> {
    let filter = ui.filter.as_str();
    let mut rows = Vec::new();

    for (namespace, targets) in &tree.0 {
        let mut group_rows = Vec::new();
        let mut namespace_env_count = 0;

        for (target, envs) in targets {
            // Filters on `spec.key` only, per spec — a namespace/target whose *name* happens
            // to match the typed text is not enough to keep it visible if none of its
            // sessions' keys match.
            let matching: Vec<&Arc<PreviewSession>> = envs
                .iter()
                .filter(|session| session.spec.key.contains(filter))
                .collect();
            if matching.is_empty() {
                continue;
            }
            namespace_env_count += matching.len();

            let collapsed = ui
                .tree
                .is_collapsed(&Selection::Target(namespace.clone(), target.clone()));
            group_rows.push(Row::TargetHeader {
                namespace: namespace.clone(),
                target: target.clone(),
                collapsed,
                env_count: matching.len(),
            });
            if !collapsed {
                group_rows.extend(matching.into_iter().map(|session| Row::Env {
                    namespace: namespace.clone(),
                    target: target.clone(),
                    session: session.clone(),
                }));
            }
        }

        if group_rows.is_empty() {
            continue;
        }

        let collapsed = ui
            .tree
            .is_collapsed(&Selection::Namespace(namespace.clone()));
        rows.push(Row::NamespaceHeader {
            namespace: namespace.clone(),
            collapsed,
            env_count: namespace_env_count,
        });
        if !collapsed {
            rows.extend(group_rows);
        }
    }

    rows
}

/// Columns of the table, in order: the tree of names, then each environment's details.
const COLUMNS: [(&str, Constraint); 5] = [
    ("NAME", Constraint::Fill(3)),
    ("PHASE", Constraint::Length(12)),
    ("STATUS", Constraint::Fill(3)),
    ("MODE", Constraint::Length(6)),
    ("IMAGE", Constraint::Fill(2)),
];

/// Draws the screen: a `previews[N]` panel holding the table, the filter or go-to line under it
/// while one is in use, and the stop confirmation over everything while it is open.
pub fn draw(frame: &mut Frame, area: Rect, tree: &PreviewEnvsTree, ui: &mut UiState) {
    let rows = flatten(tree, ui);
    let total: usize = tree
        .0
        .values()
        .flat_map(|targets| targets.values())
        .map(Vec::len)
        .sum();
    let shown = rows
        .iter()
        .filter(|row| matches!(row, Row::Env { .. }))
        .count();
    let count = if shown < total {
        format!("{shown}/{total}")
    } else {
        total.to_string()
    };

    let bar_height =
        u16::from(!ui.filter.is_empty() || matches!(ui.mode, Mode::Filtering | Mode::GoTo { .. }));
    let [body, bar] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(bar_height)]).areas(area);

    let block = panel::block("previews", Some(&count), None, None);
    let inner = block.inner(body);
    frame.render_widget(block, body);

    match &ui.mode {
        Mode::GoTo { input, found, .. } => draw_goto_bar(frame, bar, input, *found),
        mode if bar_height > 0 => {
            draw_search_bar(frame, bar, &ui.filter, matches!(mode, Mode::Filtering));
        }
        _ => {}
    }

    match ui.reconcile_selection(&rows) {
        None => {
            let message = if ui.filter.is_empty() {
                "No preview environments"
            } else {
                "No preview environments match the filter"
            };
            state_view::draw_message(frame, inner, message, theme::muted());
        }
        Some(selected) => draw_table(frame, inner, &rows, selected, ui),
    }

    if let Mode::ConfirmStop {
        scope,
        candidates,
        confirmation,
    } = &ui.mode
    {
        draw_confirm_stop(frame, area, scope, candidates, confirmation);
    }
}

/// The header row and as many rows as fit, scrolled just enough to keep `selected` in view.
fn draw_table(frame: &mut Frame, area: Rect, rows: &[Row], selected: usize, ui: &mut UiState) {
    let [header, list] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    let widths = Layout::horizontal(COLUMNS.map(|(_, width)| width))
        .spacing(1)
        .split(Rect {
            height: 1,
            ..header
        });

    let header_line = Line::from(
        COLUMNS
            .iter()
            .zip(widths.iter())
            .flat_map(|((title, _), rect)| {
                [
                    Span::styled(
                        format!(
                            "{:<width$}",
                            ellipsize(title, rect.width.into()),
                            width = rect.width.into()
                        ),
                        theme::table_header(),
                    ),
                    Span::raw(" "),
                ]
            })
            .collect::<Vec<_>>(),
    );
    frame.render_widget(Paragraph::new(header_line), header);

    let height = usize::from(list.height).max(1);
    if selected < ui.scroll_offset {
        ui.scroll_offset = selected;
    } else if selected >= ui.scroll_offset + height {
        ui.scroll_offset = selected + 1 - height;
    }
    ui.scroll_offset = ui.scroll_offset.min(rows.len().saturating_sub(height));

    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .skip(ui.scroll_offset)
        .take(height)
        .map(|(index, row)| {
            let cells = cells(row);
            let mut spans = Vec::with_capacity(cells.len() * 2);
            for ((text, style), rect) in cells.into_iter().zip(widths.iter()) {
                let width = usize::from(rect.width);
                spans.push(Span::styled(
                    format!("{:<width$}", ellipsize(&text, width)),
                    style,
                ));
                spans.push(Span::raw(" "));
            }
            let line = Line::from(spans);
            if index == selected {
                line.style(theme::selection())
            } else {
                line
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), list);
}

/// The cells of one row, in [`COLUMNS`] order. Group rows only fill the name column.
fn cells(row: &Row) -> Vec<(String, Style)> {
    let arrow = |collapsed: bool| if collapsed { "\u{25b8}" } else { "\u{25be}" };
    let empty = || (String::new(), Style::default());

    match row {
        Row::NamespaceHeader {
            namespace,
            collapsed,
            env_count,
        } => vec![
            (
                format!("{} {namespace} ({env_count})", arrow(*collapsed)),
                Style::default()
                    .fg(brand::NAMESPACE)
                    .add_modifier(Modifier::BOLD),
            ),
            empty(),
            empty(),
            empty(),
            empty(),
        ],
        Row::TargetHeader {
            target,
            collapsed,
            env_count,
            ..
        } => vec![
            (
                format!(
                    "  {} {}/{} ({env_count})",
                    arrow(*collapsed),
                    target.0,
                    target.1
                ),
                Style::default().fg(brand::TARGET),
            ),
            empty(),
            empty(),
            empty(),
            empty(),
        ],
        Row::Env { session, .. } => {
            let phase = session
                .status
                .as_ref()
                .map(|status| status.phase.to_string())
                .unwrap_or_else(|| "Unknown".to_owned());
            let mode = match &session.spec.incoming {
                Some(incoming) if incoming.steal => "steal",
                Some(_) => "mirror",
                None => "-",
            };
            vec![
                (
                    format!("      {}", session.spec.key),
                    Style::default().fg(theme::LAVENDER),
                ),
                (phase, Style::default().fg(phase_color(session))),
                (status_text(session), Style::default()),
                (mode.to_owned(), theme::muted()),
                (session.spec.image.clone(), theme::muted()),
            ]
        }
    }
}

/// The filter line under the table: `/`, the filter typed so far, and the cursor while typing.
fn draw_search_bar(frame: &mut Frame, area: Rect, filter: &str, editing: bool) {
    let mut spans = vec![
        Span::styled("/", theme::title()),
        Span::styled(filter.to_owned(), Style::default().fg(theme::LAVENDER)),
    ];
    if editing {
        spans.push(Span::styled("\u{2588}", theme::title()));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The go-to line under the table, the typed text red while it matches no namespace or target.
fn draw_goto_bar(frame: &mut Frame, area: Rect, input: &str, found: bool) {
    let color = if found { theme::LAVENDER } else { brand::RED };
    let spans = vec![
        Span::styled("go to ", theme::title()),
        Span::styled(input.to_owned(), Style::default().fg(color)),
        Span::styled("\u{2588}", theme::title()),
    ];
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The `s` stop-confirmation dialog: a centered modal, red-bordered to signal it's destructive,
/// listing exactly which preview environment(s) `Enter` would delete. Drawn last (on top of
/// everything else) and, per `PreviewEnvsScreen::handle_event`, captures all input except
/// `Enter`/`Esc` while open — nothing is deleted until the user explicitly confirms.
fn draw_confirm_stop(
    frame: &mut Frame,
    area: Rect,
    scope: &Selection,
    candidates: &[StopCandidate],
    confirmation: &str,
) {
    let lines = confirm_stop_lines(scope, candidates, confirmation);
    let content_width = lines.iter().map(Line::width).max().unwrap_or(0) as u16;
    let width = (content_width + 2).min(area.width.saturating_sub(2));
    let height = (lines.len() as u16 + 2).min(area.height.saturating_sub(2));
    let dialog_area = centered(area, Constraint::Length(width), Constraint::Length(height));

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(brand::RED))
        .title(Span::styled(
            " stop preview environment(s) ",
            Style::default().fg(brand::RED).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(dialog_area);

    frame.render_widget(Clear, dialog_area);
    frame.render_widget(block, dialog_area);
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// Content of the stop-confirmation dialog. The headline is unambiguous about scope — plural,
/// bold, and red specifically when it covers more than one environment — so stopping every env
/// under a target/namespace never reads the same as stopping a single one. A bulk scope
/// additionally requires typing its confirmation word before `Enter` does anything (see
/// `Selection::stop_confirmation_word`); the typed-so-far text renders in red until it's an
/// exact match, then green, so it's visually obvious when `Enter` is actually armed.
fn confirm_stop_lines(
    scope: &Selection,
    candidates: &[StopCandidate],
    confirmation: &str,
) -> Vec<Line<'static>> {
    let plural = candidates.len() != 1;
    let headline = match scope {
        Selection::Env(..) => {
            // `request_stop` only opens the dialog with at least one candidate.
            let key = candidates
                .first()
                .map(|env| env.key.as_str())
                .unwrap_or_default();
            format!("Stop preview environment \"{key}\"?")
        }
        Selection::Target(namespace, target) => format!(
            "Stop ALL {} preview environments under {}/{} in namespace \"{namespace}\"?",
            candidates.len(),
            target.0,
            target.1,
        ),
        Selection::Namespace(namespace) => format!(
            "Stop ALL {} preview environments in namespace \"{namespace}\"?",
            candidates.len(),
        ),
    };
    let headline_style = if plural {
        Style::default().fg(brand::RED).add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    };

    let mut lines = vec![
        Line::from(Span::styled(headline, headline_style)),
        Line::from(""),
    ];
    for candidate in candidates {
        lines.push(Line::from(format!(
            "  - {} ({})",
            candidate.key, candidate.namespace
        )));
    }
    lines.push(Line::from(""));

    if let Some(word) = scope.stop_confirmation_word() {
        lines.push(Line::from(format!("Type \"{word}\" below to confirm:")));
        let matched = confirmation == word;
        let text_color = if matched { brand::SUCCESS } else { brand::RED };
        lines.push(Line::from(vec![
            Span::styled(confirmation.to_owned(), Style::default().fg(text_color)),
            Span::styled("\u{2588}", Style::default().fg(text_color)),
        ]));
        lines.push(Line::from(""));
    }

    lines.push(Line::from(vec![
        Span::styled(
            " enter ",
            Style::default()
                .fg(brand::INK)
                .bg(brand::YELLOW)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" stop   "),
        Span::styled(
            " esc ",
            Style::default()
                .fg(brand::INK)
                .bg(brand::YELLOW)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" cancel"),
    ]));

    lines
}

/// Mirrors the exact phrasing `mirrord preview status` uses (`mirrord/mirrord/cli/src/preview.rs`),
/// so the TUI's wording stays consistent with the CLI.
fn status_text(session: &PreviewSession) -> String {
    let Some(status) = &session.status else {
        return "pending".to_owned();
    };

    match status.phase {
        PreviewSessionPhase::Initializing => "initializing".to_owned(),
        PreviewSessionPhase::Waiting => "waiting".to_owned(),
        PreviewSessionPhase::Ready => {
            if session.spec.has_infinite_ttl() {
                "running (infinite)".to_owned()
            } else {
                let remaining = status
                    .expires_at
                    .as_ref()
                    .and_then(|expires_at| {
                        std::time::Duration::try_from(
                            expires_at
                                .0
                                .duration_since(k8s_openapi::jiff::Timestamp::now()),
                        )
                        .ok()
                    })
                    .map(|duration| std::time::Duration::from_secs(duration.as_secs()));
                match remaining {
                    Some(duration) => {
                        format!(
                            "running ({} remaining)",
                            humantime::format_duration(duration)
                        )
                    }
                    None => "running".to_owned(),
                }
            }
        }
        PreviewSessionPhase::Idle => "idle (waiting for traffic)".to_owned(),
        PreviewSessionPhase::Paused => "paused".to_owned(),
        PreviewSessionPhase::Failed => status
            .failure_message
            .clone()
            .unwrap_or_else(|| "failed".to_owned()),
        PreviewSessionPhase::Unknown => "unknown".to_owned(),
    }
}
