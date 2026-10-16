//! The Home view: a dashboard of what mirrord is doing in the cluster right now.
//!
//! One card each for sessions, preview environments, queue splits and database branches, with
//! counts and what needs attention (failed splits, yours among the sessions), the operator's
//! version and license, and a feed of what happened recently, newest first. A number key jumps to
//! the view behind a card.

use std::{
    cmp::Reverse,
    ops::ControlFlow,
    sync::{Arc, RwLock},
    time::Duration,
};

use crossterm::event::{Event, KeyCode};
use k8s_openapi::{apimachinery::pkg::apis::meta::v1::Time, jiff::Timestamp};
use kube::{Api, Client, ResourceExt};
use mirrord_operator::crd::{
    MirrordOperatorCrd, OPERATOR_STATUS_NAME, SessionCrd,
    db_branching::branch_database::BranchDatabase, preview::PreviewSession,
    queue_split::QueueSplit,
};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Padding, Paragraph},
};
use tokio::time::MissedTickBehavior;

use crate::{
    context::Context,
    kit::{hints::Hint, panel, state_view},
    screens::Screen,
    theme,
};

/// How often the dashboard is refreshed.
const REFRESH_INTERVAL: Duration = Duration::from_secs(5);

/// Most entries the activity feed keeps.
const ACTIVITY_LIMIT: usize = 20;

/// Days before the license expires at which the operator card turns amber.
const LICENSE_WARNING_DAYS: i64 = 14;

/// The views behind the cards, by the key that opens them and the name `:` takes for them.
const JUMPS: [(char, &str); 5] = [
    ('1', "sessions"),
    ('2', "previews"),
    ('3', "queues"),
    ('4', "dbs"),
    ('r', "run"),
];

/// Counts for one card: how many there are, and a breakdown worth a glance.
#[derive(Clone, Debug, Default)]
struct Tally {
    total: usize,
    /// Parts of the total, such as `3 ready`, each with its color.
    parts: Vec<(String, Style)>,
}

/// Something that happened, for the activity feed.
#[derive(Clone, Debug)]
struct Happening {
    at: Timestamp,
    text: String,
    style: Style,
}

/// What the dashboard last read. Each part is read on its own, so a resource the cluster does
/// not have (no preview support, say) leaves its card empty rather than the whole dashboard.
#[derive(Default)]
struct Summary {
    sessions: Option<Result<Tally, String>>,
    previews: Option<Result<Tally, String>>,
    splits: Option<Result<Tally, String>>,
    branches: Option<Result<Tally, String>>,
    operator: Option<Result<Vec<(String, Style)>, String>>,
    activity: Vec<Happening>,
    connected: bool,
}

pub struct HomeScreen {
    summary: Arc<RwLock<Summary>>,
    view_request: Option<&'static str>,
}

/// How many of each thing the dashboard counted, for the sidebar. `None` until it is read, or
/// when the cluster could not say.
#[derive(Clone, Copy, Debug, Default)]
pub struct Counts {
    pub sessions: Option<usize>,
    pub previews: Option<usize>,
    pub splits: Option<usize>,
    pub branches: Option<usize>,
}

impl HomeScreen {
    pub fn counts(&self) -> Counts {
        let Ok(summary) = self.summary.read() else {
            return Counts::default();
        };
        let total = |tally: &Option<Result<Tally, String>>| {
            tally
                .as_ref()
                .and_then(|tally| tally.as_ref().ok())
                .map(|tally| tally.total)
        };
        Counts {
            sessions: total(&summary.sessions),
            previews: total(&summary.previews),
            splits: total(&summary.splits),
            branches: total(&summary.branches),
        }
    }
}

impl HomeScreen {
    async fn run(mut context: Context, summary: Arc<RwLock<Summary>>) {
        let mut interval = tokio::time::interval(REFRESH_INTERVAL);
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                changed = context.client.changed() => if changed.is_err() { break },
                changed = context.scope.changed() => if changed.is_err() { break },
                _ = interval.tick() => {},
            }

            let client = context
                .client
                .borrow_and_update()
                .as_ref()
                .and_then(|result| result.as_ref().ok())
                .cloned();
            let namespace = context.scope.borrow_and_update().namespace.clone();
            let fresh = match client {
                Some(client) => read(&client, namespace.as_deref()).await,
                None => Summary::default(),
            };
            if let Ok(mut slot) = summary.write() {
                *slot = fresh;
            }
            context.request_redraw();
        }
    }
}

/// Lists `T` in `namespace`, or everywhere when none is picked.
fn api<T>(client: &Client, namespace: Option<&str>) -> Api<T>
where
    T: kube::Resource<Scope = k8s_openapi::NamespaceResourceScope>,
    <T as kube::Resource>::DynamicType: Default,
{
    match namespace {
        Some(namespace) => Api::namespaced(client.clone(), namespace),
        None => Api::all(client.clone()),
    }
}

/// Reads everything the dashboard shows. Requests run one after the other: they are few, small,
/// and every five seconds at most.
async fn read(client: &Client, namespace: Option<&str>) -> Summary {
    let me = std::env::var("USER").unwrap_or_default();
    let mut activity = Vec::new();
    let failed = |what: &str, error: kube::Error| state_view::explain(&error, what);

    let sessions = match api::<SessionCrd>(client, namespace)
        .list(&Default::default())
        .await
    {
        Ok(list) => {
            let sessions: Vec<_> = list
                .into_iter()
                .map(|crd| crd.spec.session)
                .filter(|session| !session.is_preview())
                .collect();
            let mine = sessions
                .iter()
                .filter(|session| is_mine(&session.user, &me))
                .count();
            let now = Timestamp::now().as_second();
            for session in &sessions {
                let started = now - i64::try_from(session.duration_secs).unwrap_or(i64::MAX);
                if let Ok(at) = Timestamp::from_second(started) {
                    activity.push(Happening {
                        at,
                        text: format!(
                            "{} started a session on {}",
                            short_user(&session.user),
                            session.target
                        ),
                        style: Style::default(),
                    });
                }
            }
            let mut parts = Vec::new();
            if mine > 0 {
                parts.push((format!("{mine} yours"), Style::default().fg(theme::MINT)));
            }
            Ok(Tally {
                total: sessions.len(),
                parts,
            })
        }
        Err(error) => Err(failed("sessions", error)),
    };

    let previews = match api::<PreviewSession>(client, namespace)
        .list(&Default::default())
        .await
    {
        Ok(list) => {
            let mut phases: Vec<(String, usize)> = Vec::new();
            for preview in &list.items {
                let phase = preview
                    .status
                    .as_ref()
                    .map(|status| status.phase.to_string())
                    .unwrap_or_else(|| "Unknown".to_owned());
                match phases.iter_mut().find(|(name, _)| *name == phase) {
                    Some((_, count)) => *count += 1,
                    None => phases.push((phase.clone(), 1)),
                }
                if let Some(at) = created(preview.metadata.creation_timestamp.as_ref()) {
                    activity.push(Happening {
                        at,
                        text: format!(
                            "preview {} \u{b7} {}",
                            preview.spec.key,
                            phase.to_lowercase()
                        ),
                        style: phase_style(&phase),
                    });
                }
            }
            Ok(Tally {
                total: list.items.len(),
                parts: phases
                    .into_iter()
                    .map(|(phase, count)| {
                        (
                            format!("{count} {}", phase.to_lowercase()),
                            phase_style(&phase),
                        )
                    })
                    .collect(),
            })
        }
        Err(error) => Err(failed("preview environments", error)),
    };

    let splits = match api::<QueueSplit>(client, namespace)
        .list(&Default::default())
        .await
    {
        Ok(list) => {
            let failed_count = list
                .items
                .iter()
                .filter(|split| {
                    split
                        .status
                        .as_ref()
                        .is_some_and(|status| status.phase == "Failed")
                })
                .count();
            for split in &list.items {
                let phase = split
                    .status
                    .as_ref()
                    .map(|status| status.phase.clone())
                    .unwrap_or_else(|| "Pending".to_owned());
                if let Some(at) = created(split.metadata.creation_timestamp.as_ref()) {
                    activity.push(Happening {
                        at,
                        text: format!(
                            "queue split {} \u{b7} {}",
                            split.name_any(),
                            phase.to_lowercase()
                        ),
                        style: phase_style(&phase),
                    });
                }
            }
            let mut parts = Vec::new();
            if failed_count > 0 {
                parts.push((format!("{failed_count} failed"), theme::error()));
            }
            Ok(Tally {
                total: list.items.len(),
                parts,
            })
        }
        Err(error) => Err(failed("queue splits", error)),
    };

    let branches = match api::<BranchDatabase>(client, namespace)
        .list(&Default::default())
        .await
    {
        Ok(list) => {
            let mut phases: Vec<(String, usize)> = Vec::new();
            for branch in &list.items {
                let phase = branch
                    .status
                    .as_ref()
                    .map(|status| format!("{:?}", status.phase))
                    .unwrap_or_else(|| "Pending".to_owned());
                match phases.iter_mut().find(|(name, _)| *name == phase) {
                    Some((_, count)) => *count += 1,
                    None => phases.push((phase.clone(), 1)),
                }
                if let Some(at) = created(branch.metadata.creation_timestamp.as_ref()) {
                    activity.push(Happening {
                        at,
                        text: format!(
                            "db branch {} \u{b7} {}",
                            branch.spec.id,
                            phase.to_lowercase()
                        ),
                        style: phase_style(&phase),
                    });
                }
            }
            Ok(Tally {
                total: list.items.len(),
                parts: phases
                    .into_iter()
                    .map(|(phase, count)| {
                        (
                            format!("{count} {}", phase.to_lowercase()),
                            phase_style(&phase),
                        )
                    })
                    .collect(),
            })
        }
        Err(error) => Err(failed("database branches", error)),
    };

    let operator = match Api::<MirrordOperatorCrd>::all(client.clone())
        .get(OPERATOR_STATUS_NAME)
        .await
    {
        Ok(operator) => Ok(operator_lines(&operator)),
        Err(error) => Err(failed("the operator's status", error)),
    };

    activity.sort_by_key(|happening| Reverse(happening.at));
    activity.truncate(ACTIVITY_LIMIT);

    Summary {
        sessions: Some(sessions),
        previews: Some(previews),
        splits: Some(splits),
        branches: Some(branches),
        operator: Some(operator),
        activity,
        connected: true,
    }
}

/// The operator card: its version, and whose license it runs on until when.
fn operator_lines(operator: &MirrordOperatorCrd) -> Vec<(String, Style)> {
    let license = &operator.spec.license;
    let expires = license.expire_at.to_string();
    let days_left = expires
        .parse::<k8s_openapi::jiff::civil::Date>()
        .ok()
        .and_then(|date| {
            let today = k8s_openapi::jiff::Zoned::now().date();
            today
                .until(date)
                .ok()
                .map(|span| i64::from(span.get_days()))
        });

    let license_style = match days_left {
        Some(days) if days < 0 => theme::error(),
        Some(days) if days < LICENSE_WARNING_DAYS => theme::warning(),
        _ => theme::muted(),
    };
    let left = match days_left {
        Some(days) if days < 0 => " (expired)".to_owned(),
        Some(days) => format!(" ({days} days left)"),
        None => String::new(),
    };

    vec![
        (
            format!("operator {}", operator.spec.operator_version),
            Style::default()
                .fg(theme::LAVENDER)
                .add_modifier(Modifier::BOLD),
        ),
        (
            format!(
                "license {} \u{b7} until {expires}{left}",
                license.organization
            ),
            license_style,
        ),
    ]
}

/// Whether a session's user is the person running the TUI: one of the parts of
/// `username/k8sUsername@hostname` is their login name.
fn is_mine(user: &str, me: &str) -> bool {
    !me.is_empty()
        && user
            .split(['/', '@'])
            .any(|part| part.eq_ignore_ascii_case(me))
}

/// The login name part of a session user, for the feed.
fn short_user(user: &str) -> &str {
    user.split(['/', '@'])
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(user)
}

fn created(time: Option<&Time>) -> Option<Timestamp> {
    time.map(|time| time.0)
}

fn phase_style(phase: &str) -> Style {
    match phase.to_ascii_lowercase().as_str() {
        "ready" => Style::default().fg(theme::MINT),
        "failed" => theme::error(),
        "idle" | "paused" | "pending" | "init" | "initializing" | "waiting" => theme::warning(),
        _ => theme::muted(),
    }
}

/// How long ago `at` was, as `kubectl` prints ages.
fn ago(at: Timestamp) -> String {
    let seconds = (Timestamp::now().as_second() - at.as_second()).max(0);
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds / 60),
        3600..86400 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86400),
    }
}

/// A count card: the key that opens its view, its name, the count large, and the breakdown.
fn count_card(
    frame: &mut Frame,
    area: Rect,
    key: char,
    name: &str,
    tally: Option<&Result<Tally, String>>,
) {
    let lines = match tally {
        None => vec![Line::styled("...", theme::muted())],
        Some(Err(error)) => vec![Line::styled(error.clone(), theme::muted())],
        Some(Ok(tally)) => {
            let mut parts = vec![Span::styled(
                tally.total.to_string(),
                Style::default()
                    .fg(theme::LAVENDER)
                    .add_modifier(Modifier::BOLD),
            )];
            for (text, style) in &tally.parts {
                parts.push(Span::styled("  \u{b7}  ", theme::muted()));
                parts.push(Span::styled(text.clone(), *style));
            }
            vec![Line::from(parts)]
        }
    };
    card(frame, area, Some(key), name, lines);
}

fn card(frame: &mut Frame, area: Rect, key: Option<char>, name: &str, lines: Vec<Line<'static>>) {
    let mut title = vec![Span::raw(" ")];
    if let Some(key) = key {
        title.push(Span::styled(
            format!("<{key}> "),
            Style::default()
                .fg(theme::INDIGO)
                .add_modifier(Modifier::BOLD),
        ));
    }
    title.push(Span::styled(name.to_uppercase(), theme::table_header()));
    title.push(Span::raw(" "));
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::BORDER_DIM))
        .padding(Padding::horizontal(1))
        .title(Line::from(title));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(lines), inner);
}

impl Screen for HomeScreen {
    fn new(context: Context) -> Self {
        let summary = Arc::new(RwLock::new(Summary::default()));
        tokio::spawn(Self::run(context, summary.clone()));
        Self {
            summary,
            view_request: None,
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let block = panel::block("home", None, None, None);
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let Ok(summary) = self.summary.read() else {
            return;
        };
        if !summary.connected {
            state_view::draw_message(
                frame,
                inner,
                "Waiting for a connection to the cluster...",
                theme::muted(),
            );
            return;
        }

        // Two cards to a row when there is room, one otherwise; the feed takes what is left.
        let wide = inner.width >= 70;
        let counts_height: u16 = if wide { 6 } else { 12 };
        let [counts, operator, activity] = Layout::vertical([
            Constraint::Length(counts_height),
            Constraint::Length(4),
            Constraint::Min(3),
        ])
        .areas(inner);

        let cells: Vec<Rect> = if wide {
            let [top, bottom] =
                Layout::vertical([Constraint::Length(3), Constraint::Length(3)]).areas(counts);
            let halves = |row: Rect| {
                Layout::horizontal([Constraint::Fill(1), Constraint::Fill(1)])
                    .spacing(1)
                    .areas::<2>(row)
            };
            let [a, b] = halves(top);
            let [c, d] = halves(bottom);
            vec![a, b, c, d]
        } else {
            Layout::vertical([Constraint::Length(3); 4])
                .split(counts)
                .to_vec()
        };
        let mut cells = cells.into_iter();
        for (key, name, tally) in [
            ('1', "sessions", summary.sessions.as_ref()),
            ('2', "previews", summary.previews.as_ref()),
            ('3', "queue splits", summary.splits.as_ref()),
            ('4', "db branches", summary.branches.as_ref()),
        ] {
            if let Some(cell) = cells.next() {
                count_card(frame, cell, key, name, tally);
            }
        }

        let operator_lines = match &summary.operator {
            None => vec![Line::styled("...", theme::muted())],
            Some(Err(error)) => vec![Line::styled(error.clone(), theme::warning())],
            Some(Ok(lines)) => lines
                .iter()
                .map(|(text, style)| Line::styled(text.clone(), *style))
                .collect(),
        };
        card(frame, operator, None, "operator", operator_lines);

        let feed: Vec<Line> = if summary.activity.is_empty() {
            vec![Line::styled("Nothing has happened yet.", theme::muted())]
        } else {
            summary
                .activity
                .iter()
                .map(|event| {
                    Line::from_iter([
                        Span::styled(format!("{:>4}  ", ago(event.at)), theme::muted()),
                        Span::styled(event.text.clone(), event.style),
                    ])
                })
                .collect()
        };
        card(frame, activity, None, "recent activity", feed);
    }

    fn handle_event(&mut self, event: Event) -> ControlFlow<(), Event> {
        let Event::Key(key) = &event else {
            return ControlFlow::Continue(event);
        };
        let KeyCode::Char(char) = key.code else {
            return ControlFlow::Continue(event);
        };
        match JUMPS.iter().find(|(jump, _)| *jump == char) {
            Some((_, view)) => {
                self.view_request = Some(view);
                ControlFlow::Break(())
            }
            None => ControlFlow::Continue(event),
        }
    }

    fn hints(&self) -> Vec<Hint> {
        vec![
            Hint::new("1", "sessions"),
            Hint::new("2", "previews"),
            Hint::new("3", "queues"),
            Hint::new("4", "dbs"),
            Hint::new("r", "run"),
        ]
    }

    fn take_view_request(&mut self) -> Option<&'static str> {
        self.view_request.take()
    }
}

#[cfg(test)]
mod tests {
    use super::{is_mine, short_user};

    #[test]
    fn a_session_is_mine_when_any_part_of_its_user_is_my_login() {
        assert!(is_mine("vlad/vlad@laptop", "vlad"));
        assert!(is_mine("alice/vlad@ci", "vlad"));
        assert!(!is_mine("vladimir/vladimir@laptop", "vlad"));
        assert!(!is_mine("vlad/vlad@laptop", ""));
        assert_eq!(short_user("vlad/k8s-vlad@laptop"), "vlad");
    }
}
