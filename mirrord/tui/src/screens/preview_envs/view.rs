//! Flattening the grouped tree into visible rows, and rendering: manual list-windowing/scroll
//! (ratatui's `List` can't render a `Block::bordered()` per item, which the "envs as
//! rectangles" requirement needs), card content, and the MetalBear-brand phase colors /
//! always-visible key-hint bar.

use std::sync::Arc;

use kube::ResourceExt;
use mirrord_operator::crd::preview::{
    PreviewDbBranchingConfig, PreviewQueueSplittingConfig, PreviewSession, PreviewSessionPhase,
};
use ratatui::{
    Frame,
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Clear, Paragraph, Widget, Wrap},
};

use super::{
    data::PreviewEnvsTree,
    ui::{Mode, Selection, StopCandidate, TargetKey, UiState},
};
use crate::helpers::centered;

/// MetalBear-inspired colors. When the terminal appears to support 24-bit truecolor, these
/// resolve to the real brand hex values; otherwise they fall back to ratatui's standard named
/// (4-bit ANSI) palette, which every terminal understands. The fallback exists because on a
/// terminal that doesn't support truecolor, `Color::Rgb` was rendering as a flat gray, and
/// `Modifier::BOLD` combined with that unrecognized color was falling back to plain bright
/// white — named colors combine with `BOLD` the standard way (bold = the bright variant of
/// that same color) instead.
mod brand {
    use std::sync::LazyLock;

    use ratatui::style::Color;

    /// Whether the terminal appears to support 24-bit truecolor, checked once via the
    /// `COLORTERM` environment variable — the de facto signal most truecolor-capable
    /// terminals set (`truecolor` or `24bit`), and the same heuristic widely used by other
    /// terminal tools (bat, delta, starship, ...). There's no reliable, low-latency way to
    /// directly query a terminal's color support via escape sequences, so this environment
    /// heuristic is the practical standard.
    static TRUECOLOR: LazyLock<bool> = LazyLock::new(|| {
        std::env::var("COLORTERM").is_ok_and(|value| {
            let value = value.to_ascii_lowercase();
            value.contains("truecolor") || value.contains("24bit")
        })
    });

    fn pick(hex: (u8, u8, u8), fallback: Color) -> Color {
        if *TRUECOLOR {
            Color::Rgb(hex.0, hex.1, hex.2)
        } else {
            fallback
        }
    }

    pub fn success() -> Color {
        pick((0x7D, 0xD3, 0xA8), Color::LightGreen) // --code-string, labeled "success". Ready
    }
    pub fn yellow() -> Color {
        pick((0xFF, 0xCB, 0x7D), Color::LightYellow) // --mb-yellow. Idle; hint-bar chip bg
    }
    pub fn purple() -> Color {
        pick((0x75, 0x6D, 0xF3), Color::LightMagenta) // --mb-purple. Waiting; focus accents
    }
    pub fn purple_dim() -> Color {
        pick((0x7A, 0x78, 0xA0), Color::Magenta) // --code-comment. Initializing
    }
    pub fn red() -> Color {
        pick((0xFF, 0x5F, 0x57), Color::LightRed) // terminal traffic-light red. Failed
    }
    pub fn grey() -> Color {
        pick((0x88, 0x88, 0x99), Color::DarkGray) // --mb-grey-500. Unknown; dim text
    }
    pub fn ink() -> Color {
        pick((0x23, 0x21, 0x41), Color::Black) // --mb-ink. hint-bar chip text
    }

    /// Fixed (non-rotating) chrome color for every namespace box. The brand's own palette has
    /// only three hues (purple, yellow, mint) plus ink/grey neutrals, and all of them are
    /// already spoken for by the phase colors above, so this deliberately steps outside the
    /// literal brand token list rather than reuse a phase color for an unrelated meaning.
    /// Chosen to read as clearly distinct from every phase color while still fitting the
    /// app's purple-forward aesthetic.
    pub fn namespace() -> Color {
        pick((0x5B, 0x9D, 0xF3), Color::LightBlue)
    }

    /// Fixed (non-rotating) chrome color for every target box — see `namespace()`.
    pub fn target() -> Color {
        pick((0x4D, 0xB6, 0xB0), Color::LightCyan)
    }

    /// A subtle background tint for the focused env card, on top of its already-bold
    /// border/title — dark and low-contrast in truecolor (close to `--mb-ink-band`, the
    /// brand's own darker navy band) so it reads as "highlighted" without fighting the
    /// phase-colored border or text drawn over it. `DarkGray` in the ANSI fallback for the
    /// same reason: most terminals default to a dark background, so a dark-gray tint still
    /// reads as lighter than the surroundings.
    pub fn focus_background() -> Color {
        pick((0x2B, 0x27, 0x57), Color::DarkGray)
    }
}

fn phase_color(session: &PreviewSession) -> Color {
    match session.status.as_ref().map(|status| status.phase) {
        Some(PreviewSessionPhase::Ready) => brand::success(),
        Some(PreviewSessionPhase::Idle) => brand::yellow(),
        Some(PreviewSessionPhase::Waiting) => brand::purple(),
        Some(PreviewSessionPhase::Initializing) => brand::purple_dim(),
        Some(PreviewSessionPhase::Failed) => brand::red(),
        Some(PreviewSessionPhase::Unknown) | None => brand::grey(),
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
                .collapsed_targets
                .contains(&(namespace.clone(), target.clone()));
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

        let collapsed = ui.collapsed_namespaces.contains(namespace);
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

/// A leaf env card, ready to render, with its focus and expanded state already resolved.
/// `expanded` (toggled explicitly with `Enter`) drives the card's size and detail; `focused`
/// (the cursor position) only affects its border emphasis — deliberately decoupled, since
/// auto-expanding on focus alone shifted every card below it on every single navigation step.
struct RenderEnv {
    session: Arc<PreviewSession>,
    focused: bool,
    expanded: bool,
}

/// A target box: a colorful rounded rectangle containing its env cards.
struct RenderTarget {
    target: TargetKey,
    collapsed: bool,
    focused: bool,
    env_count: usize,
    envs: Vec<RenderEnv>,
}

/// A namespace box: a colorful rounded rectangle containing its target boxes.
struct RenderNamespace {
    namespace: String,
    collapsed: bool,
    focused: bool,
    env_count: usize,
    targets: Vec<RenderTarget>,
}

/// A little breathing room between sibling boxes at every level (env cards within a target,
/// target boxes within a namespace, namespace boxes within the whole list) — one blank row,
/// only *between* items (never a trailing gap after the last one, which would just look like
/// wasted space at the bottom of the parent box).
const GAP: u16 = 1;

fn gaps_between(count: usize) -> u16 {
    GAP * count.saturating_sub(1) as u16
}

impl RenderEnv {
    /// 2 border rows plus exactly as many content lines as `card_content` will actually
    /// render — not a fixed guess, so an expanded card that has little to show (most fields
    /// are optional and often unset) doesn't reserve a bunch of blank space for nothing.
    fn height(&self) -> u16 {
        2 + card_content(&self.session, self.expanded).len() as u16
    }
}

impl RenderTarget {
    fn height(&self) -> u16 {
        if self.collapsed {
            2
        } else {
            let envs: u16 = self.envs.iter().map(RenderEnv::height).sum();
            2 + envs + gaps_between(self.envs.len())
        }
    }
}

impl RenderNamespace {
    fn height(&self) -> u16 {
        if self.collapsed {
            2
        } else {
            let targets: u16 = self.targets.iter().map(RenderTarget::height).sum();
            2 + targets + gaps_between(self.targets.len())
        }
    }
}

/// Re-groups the flat, already-filtered `rows` (the single source of truth for what's
/// visible — see `flatten()`) back into the namespace/target/env nesting for rendering,
/// resolving each row's focus state from `selected_index` along the way. Returns the
/// namespaces plus the index of whichever one contains the focused row, for windowing.
///
/// Relies on the ordering invariant `flatten()` guarantees: a namespace header is always
/// immediately followed by its own target headers and env rows before the next namespace
/// header appears.
fn group_rows(rows: &[Row], selected_index: usize, ui: &UiState) -> (Vec<RenderNamespace>, usize) {
    let mut namespaces: Vec<RenderNamespace> = Vec::new();
    let mut selected_namespace_index = 0;

    for (index, row) in rows.iter().enumerate() {
        let focused = index == selected_index;

        match row {
            Row::NamespaceHeader {
                namespace,
                collapsed,
                env_count,
            } => namespaces.push(RenderNamespace {
                namespace: namespace.clone(),
                collapsed: *collapsed,
                focused,
                env_count: *env_count,
                targets: Vec::new(),
            }),
            Row::TargetHeader {
                target,
                collapsed,
                env_count,
                ..
            } => namespaces
                .last_mut()
                .expect("a target row always follows its namespace header")
                .targets
                .push(RenderTarget {
                    target: target.clone(),
                    collapsed: *collapsed,
                    focused,
                    env_count: *env_count,
                    envs: Vec::new(),
                }),
            Row::Env {
                namespace,
                target,
                session,
            } => {
                let expanded = ui.expanded_envs.contains(&Selection::Env(
                    namespace.clone(),
                    target.clone(),
                    session.name_any(),
                ));
                namespaces
                    .last_mut()
                    .expect("an env row always follows its namespace header")
                    .targets
                    .last_mut()
                    .expect("an env row always follows its target header")
                    .envs
                    .push(RenderEnv {
                        session: session.clone(),
                        focused,
                        expanded,
                    });
            }
        }

        if focused {
            selected_namespace_index = namespaces.len() - 1;
        }
    }

    (namespaces, selected_namespace_index)
}

/// Renders the whole screen body: the search input (while editing), the nested namespace ->
/// target -> env boxes, and the always-visible key-hint bar.
pub fn draw(frame: &mut Frame, area: Rect, tree: &PreviewEnvsTree, ui: &mut UiState) {
    let filtering = matches!(ui.mode, Mode::Filtering);
    // The filter box stays visible once a filter is locked in too, not just while typing, so
    // the user can always see what's currently filtering — it only disappears once the filter
    // is actually cleared (Esc). A bordered box needs 3 rows (top border, content, bottom
    // border), not just the 1-line bar this used to be.
    let show_filter_box = filtering || !ui.filter.is_empty();

    let (list_area, hint_area) = if let Mode::GoTo { input, found, .. } = &ui.mode {
        // Takes the same slot the filter box uses — the two modes are mutually exclusive, so
        // there's never a conflict over it.
        let [goto, list, hints] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .areas(area);
        draw_goto_bar(frame, goto, input, *found);
        (list, hints)
    } else if show_filter_box {
        let [search, list, hints] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .areas(area);
        draw_search_bar(frame, search, &ui.filter, filtering);
        (list, hints)
    } else {
        let [list, hints] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
        (list, hints)
    };

    draw_hint_bar(frame, hint_area, ui);

    let rows = flatten(tree, ui);
    match ui.reconcile_selection(&rows) {
        None => {
            frame.render_widget(
                Paragraph::new("no preview environments").style(Style::default().fg(brand::grey())),
                list_area,
            );
        }
        Some(selected_index) => {
            let (namespaces, selected_namespace_index) = group_rows(&rows, selected_index, ui);

            let heights: Vec<usize> = namespaces
                .iter()
                .map(|namespace| namespace.height() as usize)
                .collect();
            let total_height: usize =
                heights.iter().sum::<usize>() + gaps_between(namespaces.len()) as usize;
            let viewport = list_area.height as usize;

            // Absolute [top, bottom) row range of the focused namespace within the full
            // (unclipped) content, needed to keep it in view as the scroll offset is adjusted
            // below.
            let mut cumulative = 0;
            let (mut selected_top, mut selected_bottom) = (0, 0);
            for (index, &height) in heights.iter().enumerate() {
                if index == selected_namespace_index {
                    selected_top = cumulative;
                    selected_bottom = cumulative + height;
                }
                cumulative += height;
            }

            // Scroll just enough to keep the focused namespace fully in view (or, if it's
            // taller than the viewport itself, align to its top and accept it overflows the
            // bottom — the same best-effort behavior ratatui's own `List` has for an oversized
            // single item).
            if selected_bottom - selected_top >= viewport || selected_top < ui.scroll_offset {
                ui.scroll_offset = selected_top;
            } else if selected_bottom > ui.scroll_offset + viewport {
                ui.scroll_offset = selected_bottom - viewport;
            }
            // Never scroll past the end, and don't leave blank space below content shorter
            // than the viewport — this is what actually eliminates the old per-namespace
            // pagination gaps: the canvas below is a single continuous surface, clipped to the
            // viewport at the row level rather than only ever showing whole, unclipped
            // namespace boxes.
            ui.scroll_offset = ui.scroll_offset.min(total_height.saturating_sub(viewport));

            frame.render_widget(
                ScrollableCanvas {
                    namespaces: &namespaces,
                    total_height,
                    scroll_offset: ui.scroll_offset,
                },
                list_area,
            );
        }
    }

    match &ui.mode {
        Mode::Help => draw_help(frame, area),
        Mode::ConfirmStop {
            scope,
            candidates,
            confirmation,
        } => draw_confirm_stop(frame, area, scope, candidates, confirmation),
        _ => {}
    }
}

/// Renders every namespace box into an offscreen buffer sized to their full, unclipped total
/// height, then copies only the visible row window into the real frame buffer. This is what
/// gives true continuous scrolling — a namespace box can be cut off mid-border exactly at the
/// viewport edge, like a window onto taller content, rather than being hidden in its entirety
/// whenever it doesn't fully fit (which left a blank gap at the bottom of the screen).
struct ScrollableCanvas<'a> {
    namespaces: &'a [RenderNamespace],
    total_height: usize,
    scroll_offset: usize,
}

impl Widget for ScrollableCanvas<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 || self.total_height == 0 {
            return;
        }

        let mut canvas = Buffer::empty(Rect::new(0, 0, area.width, self.total_height as u16));

        let mut y = 0u16;
        for (index, namespace) in self.namespaces.iter().enumerate() {
            if index > 0 {
                y += GAP;
            }
            let height = namespace.height();
            let namespace_area = Rect {
                x: 0,
                y,
                width: area.width,
                height,
            };
            render_namespace(&mut canvas, namespace_area, namespace);
            y += height;
        }

        let visible_rows = area
            .height
            .min((self.total_height - self.scroll_offset.min(self.total_height)) as u16);
        for row in 0..visible_rows {
            let source_y = self.scroll_offset as u16 + row;
            for col in 0..area.width {
                let Some(cell) = canvas.cell((col, source_y)) else {
                    continue;
                };
                let cell = cell.clone();
                if let Some(dest) = buf.cell_mut((area.x + col, area.y + row)) {
                    *dest = cell;
                }
            }
        }
    }
}

/// A real bordered text-box widget for the filter, shown whenever there's anything to show:
/// while actively typing (`Mode::Filtering`), and also after locking a filter in, so the
/// active filter stays visible instead of disappearing — the block cursor is the only thing
/// that distinguishes "still editing" from "locked in and browsing".
fn draw_search_bar(frame: &mut Frame, area: Rect, filter: &str, editing: bool) {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(brand::purple()))
        .title(Span::styled(
            " filter ",
            Style::default()
                .fg(brand::purple())
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut spans = vec![Span::raw(filter.to_owned())];
    if editing {
        spans.push(Span::styled(
            "\u{2588}",
            Style::default().fg(brand::purple()),
        )); // block cursor
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), inner);
}

/// A bordered text-box widget for `Mode::GoTo`'s incremental namespace/target search. Mirrors
/// `draw_search_bar`, but always shows the block cursor (it only exists while actively
/// searching — unlike the filter, there's no "locked in" state that keeps it around after),
/// and renders the typed text in red when it currently matches nothing.
fn draw_goto_bar(frame: &mut Frame, area: Rect, input: &str, found: bool) {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(brand::purple()))
        .title(Span::styled(
            " go to ",
            Style::default()
                .fg(brand::purple())
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let text_color = if found { brand::purple() } else { brand::red() };
    let spans = vec![
        Span::styled(input.to_owned(), Style::default().fg(text_color)),
        Span::styled("\u{2588}", Style::default().fg(text_color)), // block cursor
    ];
    frame.render_widget(Paragraph::new(Line::from(spans)), inner);
}

/// Always-visible legend of the keys that currently do something, styled as MetalBear
/// "eyebrow chips" (honey background, ink text, bold) — so the user never has to memorize
/// keybindings. Only advertises keys with an effect.
fn draw_hint_bar(frame: &mut Frame, area: Rect, ui: &UiState) {
    let mut spans: Vec<Span<'static>> = Vec::new();

    let mut push_chip = |key: &'static str, description: &'static str| {
        spans.push(Span::styled(
            format!(" {key} "),
            Style::default()
                .fg(brand::ink())
                .bg(brand::yellow())
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(format!(" {description}  ")));
    };

    match &ui.mode {
        Mode::Browsing => {
            push_chip("/", "search");
            push_chip("\u{2191}\u{2193} jk", "move");
            push_chip("\u{2190}\u{2192} hl", "collapse/expand");
            push_chip("n/N", "next/prev namespace");
            push_chip("t/T", "next/prev target");
            push_chip("e/E", "next/prev env");
            push_chip("g", "go to");
            push_chip("enter", "expand/collapse env");
            push_chip("s", "stop");
            push_chip("?", "help");
            if !ui.filter.is_empty() {
                push_chip("esc", "clear filter");
            }
        }
        Mode::Filtering => {
            push_chip("enter", "apply");
            push_chip("esc", "cancel");
        }
        Mode::GoTo { .. } => {
            push_chip("enter", "confirm");
            push_chip("esc", "cancel");
        }
        Mode::Help => {
            push_chip("esc", "close help");
        }
        Mode::ConfirmStop { .. } => {
            push_chip("enter", "stop");
            push_chip("esc", "cancel");
        }
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The `?` help overlay: a centered modal dialog explaining the screen. Drawn last (on top of
/// everything else) and, per `PreviewEnvsScreen::handle_event`, captures all input while open —
/// only `Esc` closes it — so it behaves like an actually-focused dialog, not a passive overlay.
fn draw_help(frame: &mut Frame, area: Rect) {
    let lines = help_lines();
    // Sized to the actual longest line (+2 for the border) rather than a guessed constant —
    // a hardcoded width previously clipped the last character or two off several lines that
    // turned out to be just barely longer than the guess.
    let content_width = lines.iter().map(Line::width).max().unwrap_or(0) as u16;
    let width = (content_width + 2).min(area.width.saturating_sub(2));
    let height = (lines.len() as u16 + 2).min(area.height.saturating_sub(2));
    let dialog_area = centered(area, Constraint::Length(width), Constraint::Length(height));

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(brand::purple()))
        .title(Span::styled(
            " help ",
            Style::default()
                .fg(brand::purple())
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(dialog_area);

    frame.render_widget(Clear, dialog_area);
    frame.render_widget(block, dialog_area);
    // A safety net in case the terminal is too narrow even for the sized-to-fit width above
    // (rare, but better to wrap onto an extra line than silently lose characters again).
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
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
        .border_style(Style::default().fg(brand::red()))
        .title(Span::styled(
            " stop preview environment(s) ",
            Style::default()
                .fg(brand::red())
                .add_modifier(Modifier::BOLD),
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
        Selection::Env(..) => format!("Stop preview environment \"{}\"?", candidates[0].key),
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
        Style::default()
            .fg(brand::red())
            .add_modifier(Modifier::BOLD)
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
        let text_color = if matched {
            brand::success()
        } else {
            brand::red()
        };
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
                .fg(brand::ink())
                .bg(brand::yellow())
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" stop   "),
        Span::styled(
            " esc ",
            Style::default()
                .fg(brand::ink())
                .bg(brand::yellow())
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" cancel"),
    ]));

    lines
}

fn help_section(title: &'static str) -> Line<'static> {
    Line::from(Span::styled(
        title,
        Style::default().add_modifier(Modifier::BOLD),
    ))
}

fn help_key_line(key: &'static str, description: &'static str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            key,
            Style::default()
                .fg(brand::purple())
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("  {description}")),
    ])
}

/// Content of the help dialog. Rebuilt fresh each time it's drawn — it's static text, but
/// cheap enough that caching it isn't worth the complexity.
fn help_lines() -> Vec<Line<'static>> {
    vec![
        help_section("Layout"),
        Line::from("Namespace and target boxes nest: namespace > target > env card."),
        Line::from("An env card's border color reflects its phase:"),
        Line::from(vec![
            Span::raw("  "),
            Span::styled("Ready", Style::default().fg(brand::success())),
            Span::raw("   "),
            Span::styled("Idle", Style::default().fg(brand::yellow())),
            Span::raw("   "),
            Span::styled("Waiting", Style::default().fg(brand::purple())),
        ]),
        Line::from(vec![
            Span::raw("  "),
            Span::styled("Initializing", Style::default().fg(brand::purple_dim())),
            Span::raw("   "),
            Span::styled("Failed", Style::default().fg(brand::red())),
            Span::raw("   "),
            Span::styled("Unknown", Style::default().fg(brand::grey())),
        ]),
        Line::from("A bold border/title marks the focused box; Enter expands an env card."),
        Line::from(""),
        help_section("Navigation"),
        help_key_line("\u{2191} / k", "move up"),
        help_key_line("\u{2193} / j", "move down"),
        help_key_line("\u{2190} / h", "collapse a header, or move to its parent"),
        help_key_line(
            "\u{2192} / l",
            "expand a header, or move to its first child",
        ),
        help_key_line("n / N", "jump to the next / previous namespace"),
        help_key_line("t / T", "jump to the next / previous target"),
        help_key_line("e / E", "jump to the next / previous env card"),
        help_key_line("g", "go to a namespace/target by name, live as you type"),
        help_key_line(
            "s",
            "stop the focused env, or ALL envs under a focused target/namespace (asks first)",
        ),
        help_key_line("Enter", "expand/collapse the focused env card's detail"),
        help_key_line(
            "/",
            "search — filters live by key substring, Enter locks it in",
        ),
        help_key_line("Esc", "clear the active filter"),
        Line::from(""),
        help_section("Global"),
        help_key_line("Tab / Shift+Tab", "switch screens"),
        help_key_line("q / Ctrl+C", "quit mirrord-tui"),
        help_key_line("?", "show this help"),
        Line::from(""),
        Line::from(Span::styled(
            "Esc closes this help.",
            Style::default().fg(brand::grey()),
        )),
    ]
}

/// Colorful bordered box shared by namespace and target groups: a rounded rectangle whose
/// title carries the collapse arrow, label, and env count. Always the same rounded border
/// type — focus is shown by bolding the border and title instead of switching to a different
/// border style.
fn group_block(label: &str, color: Color, focused: bool, collapsed: bool) -> Block<'static> {
    let arrow = if collapsed { "\u{25B8}" } else { "\u{25BE}" }; // ▸ / ▾
    let mut border_style = Style::default().fg(color);
    let mut title_style = Style::default().fg(color);
    let mut block_style = Style::default();
    if focused {
        border_style = border_style.add_modifier(Modifier::BOLD);
        title_style = title_style.add_modifier(Modifier::BOLD);
        block_style = block_style.bg(brand::focus_background());
    }

    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(border_style)
        .style(block_style)
        .title(Span::styled(format!(" {arrow} {label} "), title_style))
}

/// Renders a namespace box, then its target boxes nested inside — `Block::inner()` insets
/// each level by its own border automatically, so no manual indentation is needed. Renders
/// into a plain `Buffer` (rather than a `Frame`) so `ScrollableCanvas` can draw the whole,
/// unclipped tree into its offscreen canvas.
fn render_namespace(buf: &mut Buffer, area: Rect, namespace: &RenderNamespace) {
    let label = format!("{} ({})", namespace.namespace, namespace.env_count);
    let block = group_block(
        &label,
        brand::namespace(),
        namespace.focused,
        namespace.collapsed,
    );
    let inner = block.inner(area);
    block.render(area, buf);

    if namespace.collapsed {
        return;
    }

    let mut y = inner.y;
    for (index, target) in namespace.targets.iter().enumerate() {
        if index > 0 {
            y += GAP;
        }
        let height = target.height();
        let target_area = Rect {
            x: inner.x,
            y,
            width: inner.width,
            height,
        };
        render_target(buf, target_area, target);
        y += height;
    }
}

fn render_target(buf: &mut Buffer, area: Rect, target: &RenderTarget) {
    let color = brand::target();
    let label = format!(
        "{}/{} ({})",
        target.target.0, target.target.1, target.env_count
    );
    let block = group_block(&label, color, target.focused, target.collapsed);
    let inner = block.inner(area);
    block.render(area, buf);

    if target.collapsed {
        return;
    }

    let mut y = inner.y;
    for (index, env) in target.envs.iter().enumerate() {
        if index > 0 {
            y += GAP;
        }
        let height = env.height();
        let env_area = Rect {
            x: inner.x,
            y,
            width: inner.width,
            height,
        };
        render_card(buf, env_area, &env.session, env.focused, env.expanded);
        y += height;
    }
}

fn render_card(
    buf: &mut Buffer,
    area: Rect,
    session: &Arc<PreviewSession>,
    focused: bool,
    expanded: bool,
) {
    let color = phase_color(session);
    let mut border_style = Style::default().fg(color);
    let mut title_style = Style::default().fg(color);
    let mut block_style = Style::default();
    if focused {
        border_style = border_style.add_modifier(Modifier::BOLD);
        title_style = title_style.add_modifier(Modifier::BOLD);
        block_style = block_style.bg(brand::focus_background());
    }

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(border_style)
        .style(block_style)
        .title(Span::styled(
            format!(" {} ", session.name_any()),
            title_style,
        ));
    let inner = block.inner(area);
    block.render(area, buf);

    Paragraph::new(card_content(session, expanded)).render(inner, buf);
}

/// Builds a card's content lines. Shared by `render_card` (to actually draw them) and
/// `RenderEnv::height` (to size the card to exactly how many lines this produces, rather than
/// guessing) — most expanded-only fields are optional and frequently unset, so the line count
/// varies a lot per environment.
///
/// The env's `key` and status are the card's primary identity — shown bare, no label, the same
/// way a heading doesn't need one. Everything else is a secondary detail field, shown as a
/// dim, right-aligned label with no colon (the label's own visual weight is what separates it
/// from the value) rather than "label: value" text. The target (kind/name) is deliberately
/// never repeated here — the containing target card's title already shows it.
fn card_content(session: &PreviewSession, expanded: bool) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(Span::styled(
            session.spec.key.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(status_text(session)),
    ];

    if expanded {
        lines.push(field_line("IMAGE", session.spec.image.clone()));
        lines.push(field_line("REPLICAS", session.spec.replicas.to_string()));
        if !session.spec.target.container.is_empty() {
            lines.push(field_line(
                "CONTAINER",
                session.spec.target.container.clone(),
            ));
        }
        if let Some(share_host) = session
            .status
            .as_ref()
            .and_then(|status| status.share_host.as_deref())
        {
            lines.push(field_line("SHARE URL", format!("https://{share_host}")));
        }
        if let Some(incoming) = &session.spec.incoming {
            let mode = if incoming.steal { "steal" } else { "mirror" };
            lines.push(field_line("INCOMING", mode.to_owned()));
        }
        if let Some(queue_splitting) = &session.spec.queue_splitting {
            let count = queue_filter_count(queue_splitting);
            if count > 0 {
                lines.push(field_line("QUEUE FILTERS", count.to_string()));
            }
        }
        if let Some(db_branching) = &session.spec.db_branching {
            let count = db_branch_count(db_branching);
            if count > 0 {
                lines.push(field_line("DB BRANCHES", count.to_string()));
            }
        }
        if let Some(idle) = &session.spec.idle {
            let mut parts = Vec::new();
            if idle.start_idle {
                parts.push("starts idle".to_owned());
            }
            parts.push(match idle.sleep_after_secs {
                Some(secs) => format!(
                    "sleeps after {}",
                    humantime::format_duration(std::time::Duration::from_secs(secs))
                ),
                None => "never sleeps".to_owned(),
            });
            lines.push(field_line("IDLE", parts.join(", ")));
        }
        if let Some(status) = &session.status
            && status.phase == PreviewSessionPhase::Failed
            && let Some(message) = &status.failure_message
        {
            lines.push(field_line_styled(
                "FAILED",
                message.clone(),
                Style::default().fg(brand::red()),
            ));
        }
    }

    lines
}

/// One "card field": a dim, bold, right-aligned label (no colon) followed by its value in the
/// given style.
fn field_line_styled(label: &'static str, value: String, value_style: Style) -> Line<'static> {
    const LABEL_WIDTH: usize = 13; // width of the longest label, "QUEUE FILTERS"
    Line::from(vec![
        Span::styled(
            format!("{label:>LABEL_WIDTH$} "),
            Style::default()
                .fg(brand::grey())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(value, value_style),
    ])
}

/// `field_line_styled` with the value in the default (unstyled) text color.
fn field_line(label: &'static str, value: String) -> Line<'static> {
    field_line_styled(label, value, Style::default())
}

fn queue_filter_count(config: &PreviewQueueSplittingConfig) -> usize {
    config.sqs_queue_filters.len()
        + config.kafka_queue_filters.len()
        + config.kafka_queue_jq_filters.len()
        + config.rmq_queue_filters.len()
        + config.gcp_pubsub_queue_filters.len()
        + config.azure_service_bus_queue_filters.len()
        + config.redis_pubsub_queue_filters.len()
        + config.temporal_queue_filters.len()
        + config.bullmq_queue_filters.len()
}

fn db_branch_count(config: &PreviewDbBranchingConfig) -> usize {
    config.mysql_branch_names.len()
        + config.mariadb_branch_names.len()
        + config.pg_branch_names.len()
        + config.dynamodb_branch_names.len()
        + config.mongodb_branch_names.len()
        + config.mssql_branch_names.len()
        + config.redis_branch_names.len()
        + config.spanner_branch_names.len()
        + config.clickhouse_branch_names.len()
        + config.cockroachdb_branch_names.len()
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
        PreviewSessionPhase::Failed => status
            .failure_message
            .clone()
            .unwrap_or_else(|| "failed".to_owned()),
        PreviewSessionPhase::Unknown => "unknown".to_owned(),
    }
}
