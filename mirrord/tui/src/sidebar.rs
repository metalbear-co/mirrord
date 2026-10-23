//! The sidebar: every screen, always in view, grouped by what it is for, with live counts.
//!
//! It answers "what is there, where am I, and is anything going on" without a key to learn:
//! screens are listed by name with an icon, the current one is marked with an indigo bar, and the
//! screens that watch the cluster show how many things they hold. The cluster the interface talks
//! to, and how that connection is going, sit at the bottom. Clicking a screen goes there. On a
//! narrow terminal it folds to the icons alone.

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Paragraph},
};

use crate::theme;

/// Width of the sidebar with names.
const WIDE: u16 = 22;

/// Width of the folded sidebar: the border and a marker, an icon and a space.
const FOLDED: u16 = 5;

/// Terminal width from which the sidebar shows names; narrower, it folds to icons.
const FOLD_BELOW: u16 = 90;

/// One screen in the sidebar.
pub struct Item {
    pub icon: &'static str,
    pub label: &'static str,
    /// The heading the screen sits under, such as `watch`.
    pub group: Option<&'static str>,
    /// How many things the screen holds, when it counts something and the count is known.
    pub count: Option<usize>,
    pub active: bool,
}

/// The "only my sessions" switch, as the sidebar shows it.
#[derive(Clone, Copy, Debug)]
pub struct Switch {
    pub on: bool,
    /// How many mirrord sessions run on this machine.
    pub sessions: usize,
}

/// What a click in the sidebar landed on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Click {
    /// A screen, by its index among the items.
    Screen(usize),
    /// The "only my sessions" switch.
    Switch,
}

#[derive(Default)]
pub struct Sidebar {
    /// Where each item landed in the last frame, by its index.
    hits: Vec<(Rect, usize)>,
    /// Where the switch landed.
    switch: Option<Rect>,
}

impl Sidebar {
    /// How wide the sidebar is on a terminal `total` columns wide.
    pub fn width(total: u16) -> u16 {
        if total >= FOLD_BELOW { WIDE } else { FOLDED }
    }

    /// Draws `items` top down and `footer` at the bottom.
    pub fn draw(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        items: &[Item],
        switch: Switch,
        footer: Vec<Line<'static>>,
    ) {
        let folded = area.width < WIDE;
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme::BORDER_DIM));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let width = usize::from(inner.width);

        let mut lines = vec![
            Line::from(Span::styled(
                if folded {
                    " \u{25c6}"
                } else {
                    " \u{25c6} mirrord "
                },
                Style::default()
                    .fg(theme::LAVENDER)
                    .bg(theme::INDIGO)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::raw(""),
        ];
        self.switch = Some(Rect::new(area.x, inner.y + 2, area.width, 1));
        lines.push(switch_line(switch, folded, width));
        lines.push(Line::raw(""));
        self.hits.clear();
        let mut group = None;
        for (index, item) in items.iter().enumerate() {
            if item.group != group {
                group = item.group;
                if let Some(heading) = item.group {
                    lines.push(Line::raw(""));
                    if !folded {
                        lines.push(Line::styled(
                            format!(" {}", heading.to_uppercase()),
                            Style::default()
                                .fg(theme::INDIGO_LIGHT)
                                .add_modifier(Modifier::BOLD),
                        ));
                    }
                }
            }
            let y = inner.y + u16::try_from(lines.len()).unwrap_or(u16::MAX);
            if y >= inner.y + inner.height {
                break;
            }
            self.hits.push((Rect::new(area.x, y, area.width, 1), index));
            lines.push(item_line(item, folded, width));
        }

        let footer_height = footer.len().min(usize::from(inner.height));
        let room = usize::from(inner.height).saturating_sub(footer_height);
        lines.truncate(room);
        lines.resize(room, Line::raw(""));
        lines.extend(footer.into_iter().take(footer_height));
        frame.render_widget(Paragraph::new(lines), inner);
    }

    /// What a left click landed on: a screen or the switch.
    pub fn clicked(&self, mouse: &MouseEvent) -> Option<Click> {
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return None;
        }
        let at = Position::new(mouse.column, mouse.row);
        if self.switch.is_some_and(|area| area.contains(at)) {
            return Some(Click::Switch);
        }
        self.hits
            .iter()
            .find(|(area, _)| area.contains(at))
            .map(|(_, index)| Click::Screen(*index))
    }
}

/// The switch's row: `○ everything` while every resource shows, `◉ only mine · 2` (mint) while
/// screens narrow to the sessions running here, amber when none does, so the empty lists that
/// follow are explained; `m` at the right edge says how to flip it.
fn switch_line(switch: Switch, folded: bool, width: usize) -> Line<'static> {
    let (sign, text, color) = match (switch.on, switch.sessions) {
        (false, _) => ("\u{25cb}", "everything".to_owned(), theme::GREY),
        (true, 0) => ("\u{25c9}", "only mine \u{b7} none".to_owned(), theme::AMBER),
        (true, count) => ("\u{25c9}", format!("only mine \u{b7} {count}"), theme::MINT),
    };
    let style = Style::default().fg(color).add_modifier(if switch.on {
        Modifier::BOLD
    } else {
        Modifier::empty()
    });
    if folded {
        return Line::from(vec![Span::raw(" "), Span::styled(sign, style)]);
    }
    let text_width = width.saturating_sub(5);
    Line::from(vec![
        Span::raw(" "),
        Span::styled(format!("{sign} {text:<text_width$}"), style),
        Span::styled("m ", theme::title()),
    ])
}

/// One screen's row: the indigo bar when it is current, its icon, its name and its count at the
/// right edge.
fn item_line(item: &Item, folded: bool, width: usize) -> Line<'static> {
    let marker = Span::styled(
        if item.active { "\u{258c}" } else { " " },
        Style::default().fg(theme::INDIGO),
    );
    let icon_style = if item.active {
        Style::default()
            .fg(theme::LAVENDER)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme::INDIGO_LIGHT)
    };
    let mut spans = vec![marker, Span::styled(item.icon, icon_style)];
    if !folded {
        let count = item
            .count
            .filter(|count| *count > 0)
            .map(|count| count.to_string())
            .unwrap_or_default();
        let label_width = width.saturating_sub(4 + count.chars().count());
        spans.push(Span::styled(
            format!(" {:<label_width$}", item.label),
            if item.active {
                Style::default()
                    .fg(theme::LAVENDER)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme::GREY)
            },
        ));
        spans.push(Span::styled(
            format!("{count} "),
            Style::default()
                .fg(theme::MINT)
                .add_modifier(Modifier::BOLD),
        ));
    }
    let line = Line::from(spans);
    if item.active {
        line.style(Style::default().bg(theme::DEEP))
    } else {
        line
    }
}
