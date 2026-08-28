//! Cluster target browser: fetches everything mirrord can target in the
//! current namespace and renders it as a flat tree with per-kind badges.

use std::{
    collections::{BTreeMap, HashSet},
    sync::{Arc, OnceLock, RwLock},
    time::Instant,
};

use crossterm::event::{KeyCode, KeyEvent};
use futures_util::future::join_all;
use mirrord_config::target::TargetType;
use mirrord_kube::api::kubernetes::seeker::KubeResourceSeeker;
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, BorderType, List, ListItem, ListState},
};
use strum::VariantArray;
use strum_macros::{Display, IntoStaticStr, VariantArray};
use tokio::sync::Notify;

use crate::{
    context::Context,
    helpers::ellipsize,
    screens::targets::{keys, theme},
};

/// A kind of cluster resource mirrord can target. Declaration order is the
/// display order in the browser.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Display, IntoStaticStr, VariantArray)]
#[strum(serialize_all = "lowercase")]
pub enum TargetKind {
    Deployment,
    Rollout,
    StatefulSet,
    Pod,
    CronJob,
    Job,
    Service,
    ReplicaSet,
}

impl TargetKind {
    /// Short colored badge shown at the start of each row.
    pub fn badge(self) -> &'static str {
        match self {
            Self::Deployment => "dep",
            Self::Rollout => "ro ",
            Self::StatefulSet => "sts",
            Self::Pod => "pod",
            Self::CronJob => "cj ",
            Self::Job => "job",
            Self::Service => "svc",
            Self::ReplicaSet => "rs ",
        }
    }
}

impl From<TargetKind> for TargetType {
    fn from(kind: TargetKind) -> Self {
        match kind {
            TargetKind::Deployment => Self::Deployment,
            TargetKind::Rollout => Self::Rollout,
            TargetKind::StatefulSet => Self::StatefulSet,
            TargetKind::Pod => Self::Pod,
            TargetKind::CronJob => Self::CronJob,
            TargetKind::Job => Self::Job,
            TargetKind::Service => Self::Service,
            TargetKind::ReplicaSet => Self::ReplicaSet,
        }
    }
}

/// One targetable workload; containers are present only when the seeker
/// reports per-container paths (multi-container pods).
#[derive(Clone, Debug)]
pub struct TargetItem {
    pub name: String,
    pub containers: Vec<String>,
}

/// Listing outcome for one kind. Failures are per-kind so a missing Argo
/// Rollout CRD or an RBAC gap never blanks the rest of the browser.
pub struct KindListing {
    pub kind: TargetKind,
    pub outcome: Result<Vec<TargetItem>, String>,
}

/// What the fetch task last produced.
#[derive(Default)]
pub struct BrowserData {
    /// Namespace the listings below are for.
    pub namespace: String,
    pub kinds: Vec<KindListing>,
    pub loading: bool,
}

/// A visible row of the tree, produced fresh from the data on every draw.
pub enum Row {
    Workload {
        kind: TargetKind,
        name: String,
        containers: usize,
        expanded: bool,
    },
    Container {
        kind: TargetKind,
        workload: String,
        name: String,
        last: bool,
    },
    Targetless,
    KindError {
        kind: TargetKind,
        message: String,
    },
}

/// What a key press in the browser resulted in.
pub enum BrowserOutcome {
    Consumed,
    Ignored,
    Pick(PickedTarget),
}

/// A target the user picked, ready to seed a service draft.
pub struct PickedTarget {
    /// Full mirrord target path, e.g. `deployment/foo/container/bar`,
    /// or `targetless`.
    pub path: String,
    pub namespace: String,
    /// Workload name, used as the default service name.
    pub workload_name: String,
}

/// Braille spinner frames for loading states, advanced by wall time so
/// the app's periodic redraw animates it.
const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// The kubeconfig's current context name, for the pane title when no
/// explicit context was picked. Read once: an explicit pick goes through
/// the scope and takes precedence anyway.
fn current_context_name() -> &'static str {
    static NAME: OnceLock<String> = OnceLock::new();
    NAME.get_or_init(|| {
        kube::config::Kubeconfig::read()
            .ok()
            .and_then(|config| config.current_context)
            .unwrap_or_else(|| "current cluster".to_owned())
    })
}

pub struct Browser {
    context: Context,
    data: Arc<RwLock<BrowserData>>,
    refresh: Arc<Notify>,
    selected: usize,
    expanded: HashSet<(TargetKind, String)>,
    filter: String,
    filter_active: bool,
    list_state: ListState,
    /// Anchors the loading spinner's animation.
    started: Instant,
}

impl Browser {
    pub fn new(context: Context) -> Self {
        let data = Arc::new(RwLock::new(BrowserData {
            loading: true,
            ..Default::default()
        }));
        let refresh = Arc::new(Notify::new());

        tokio::spawn(fetch_loop(context.clone(), refresh.clone(), data.clone()));

        Self {
            context,
            data,
            refresh,
            selected: 0,
            expanded: HashSet::new(),
            filter: String::new(),
            filter_active: false,
            list_state: ListState::default(),
            started: Instant::now(),
        }
    }

    /// The spinner frame for right now.
    fn spinner(&self) -> char {
        let frame = (self.started.elapsed().as_millis() / 120) as usize % SPINNER.len();
        SPINNER[frame]
    }

    /// Builds the currently visible rows from the fetched data, the filter,
    /// and the expand state.
    fn rows(&self) -> Vec<Row> {
        let data = match self.data.read() {
            Ok(data) => data,
            Err(_) => return Vec::new(),
        };

        let mut rows = Vec::new();
        let mut errors = Vec::new();

        for listing in &data.kinds {
            let items = match &listing.outcome {
                Ok(items) => items,
                Err(message) => {
                    errors.push(Row::KindError {
                        kind: listing.kind,
                        message: message.clone(),
                    });
                    continue;
                }
            };

            for item in items {
                let name_matches = subsequence_match(&self.filter, &item.name);
                let matched_containers: Vec<&String> = item
                    .containers
                    .iter()
                    .filter(|container| subsequence_match(&self.filter, container))
                    .collect();

                if !name_matches && matched_containers.is_empty() {
                    continue;
                }

                let key = (listing.kind, item.name.clone());
                // A filter hit on a container implies the user wants to see it,
                // so force the workload open for container-only matches.
                let expanded = !item.containers.is_empty()
                    && (self.expanded.contains(&key)
                        || (!name_matches && !matched_containers.is_empty()));

                rows.push(Row::Workload {
                    kind: listing.kind,
                    name: item.name.clone(),
                    containers: item.containers.len(),
                    expanded,
                });

                if !expanded {
                    continue;
                }

                let shown: Vec<&String> = if name_matches {
                    item.containers.iter().collect()
                } else {
                    matched_containers
                };
                let count = shown.len();
                for (position, container) in shown.into_iter().enumerate() {
                    rows.push(Row::Container {
                        kind: listing.kind,
                        workload: item.name.clone(),
                        name: container.clone(),
                        last: position + 1 == count,
                    });
                }
            }
        }

        if subsequence_match(&self.filter, "targetless") {
            rows.push(Row::Targetless);
        }
        rows.extend(errors);
        rows
    }

    /// One line describing what Enter does on the selected row, for the hint bar.
    pub fn enter_hint(&self) -> String {
        let rows = self.rows();
        match rows.get(self.selected) {
            Some(Row::Workload {
                kind,
                name,
                containers,
                expanded,
            }) => {
                if *containers > 0 && !expanded {
                    format!("Enter expand {containers} containers")
                } else {
                    format!("Enter target {kind}/{name}")
                }
            }
            Some(Row::Container { workload, name, .. }) => {
                format!("Enter target {workload}, container {name}")
            }
            Some(Row::Targetless) => "Enter run targetless".to_owned(),
            Some(Row::KindError { .. }) | None => "no target here".to_owned(),
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> BrowserOutcome {
        if self.filter_active {
            match key.code {
                KeyCode::Esc => {
                    self.filter.clear();
                    self.filter_active = false;
                }
                KeyCode::Enter => self.filter_active = false,
                KeyCode::Backspace => {
                    self.filter.pop();
                }
                KeyCode::Char(c) => self.filter.push(c),
                _ => {}
            }
            self.selected = 0;
            return BrowserOutcome::Consumed;
        }

        let rows = self.rows();

        match key.code {
            KeyCode::Up | KeyCode::Char(keys::UP) => {
                self.selected = self.selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char(keys::DOWN) => {
                self.selected = (self.selected + 1).min(rows.len().saturating_sub(1));
            }
            KeyCode::Char(keys::FILTER) => self.filter_active = true,
            KeyCode::Char(keys::REFRESH) => {
                if let Ok(mut data) = self.data.write() {
                    data.loading = true;
                }
                self.refresh.notify_one();
            }
            KeyCode::Right | KeyCode::Char(keys::EXPAND) => {
                if let Some(Row::Workload {
                    kind,
                    name,
                    containers,
                    ..
                }) = rows.get(self.selected)
                    && *containers > 0
                {
                    self.expanded.insert((*kind, name.clone()));
                }
            }
            KeyCode::Left | KeyCode::Char(keys::COLLAPSE) => {
                self.collapse_selected(&rows);
            }
            // Esc walks back one step at a time, k9s-style: an applied
            // filter goes first, then the expanded workload under the
            // cursor. With nothing left to undo it bubbles up.
            KeyCode::Esc => {
                if !self.filter.is_empty() {
                    self.filter.clear();
                    self.selected = 0;
                } else if !self.collapse_selected(&rows) {
                    return BrowserOutcome::Ignored;
                }
            }
            KeyCode::Enter => return self.pick(&rows),
            _ => return BrowserOutcome::Ignored,
        }

        BrowserOutcome::Consumed
    }

    /// Collapses the workload under the cursor (also from one of its
    /// container rows); returns whether anything was collapsed.
    fn collapse_selected(&mut self, rows: &[Row]) -> bool {
        match rows.get(self.selected) {
            Some(Row::Workload { kind, name, .. }) => self.expanded.remove(&(*kind, name.clone())),
            Some(Row::Container { kind, workload, .. }) => {
                // Collapse from inside: jump back to the workload row.
                self.expanded.remove(&(*kind, workload.clone()));
                let target = (*kind, workload.clone());
                self.selected = self
                    .rows()
                    .iter()
                    .position(|row| {
                        matches!(row, Row::Workload { kind, name, .. }
                            if (*kind, name.clone()) == target)
                    })
                    .unwrap_or(0);
                true
            }
            _ => false,
        }
    }

    /// True while the filter line is capturing keystrokes.
    /// Routes a terminal paste into the filter while it is being typed;
    /// `false` otherwise. Control characters collapse to spaces and the
    /// ends trim - the filter is a single line.
    pub fn paste(&mut self, text: &str) -> bool {
        if !self.filter_active {
            return false;
        }
        let cleaned: String = text
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        self.filter.push_str(cleaned.trim());
        true
    }

    pub fn typing(&self) -> bool {
        self.filter_active
    }

    fn pick(&mut self, rows: &[Row]) -> BrowserOutcome {
        let namespace = self
            .data
            .read()
            .map(|data| data.namespace.clone())
            .unwrap_or_default();

        match rows.get(self.selected) {
            Some(Row::Workload {
                kind,
                name,
                containers,
                expanded,
            }) => {
                // First Enter on a collapsed multi-container workload expands
                // it; Enter again targets the whole workload.
                if *containers > 0 && !expanded {
                    self.expanded.insert((*kind, name.clone()));
                    return BrowserOutcome::Consumed;
                }
                BrowserOutcome::Pick(PickedTarget {
                    path: format!("{kind}/{name}"),
                    namespace,
                    workload_name: name.clone(),
                })
            }
            Some(Row::Container {
                kind,
                workload,
                name,
                ..
            }) => BrowserOutcome::Pick(PickedTarget {
                path: format!("{kind}/{workload}/container/{name}"),
                namespace,
                workload_name: workload.clone(),
            }),
            Some(Row::Targetless) => BrowserOutcome::Pick(PickedTarget {
                path: "targetless".to_owned(),
                namespace,
                workload_name: "local".to_owned(),
            }),
            Some(Row::KindError { .. }) | None => BrowserOutcome::Consumed,
        }
    }

    pub fn draw(&mut self, frame: &mut Frame, area: Rect, focused: bool) {
        let rows = self.rows();
        self.selected = self.selected.min(rows.len().saturating_sub(1));

        let (namespace, loading) = self
            .data
            .read()
            .map(|data| (data.namespace.clone(), data.loading))
            .unwrap_or_default();

        let border = if focused {
            theme::BRAND
        } else {
            theme::BORDER_DIM
        };
        // Cluster switching lives at the app level (the scope watch); the
        // title shows which cluster this listing came from.
        let cluster = self
            .context
            .scope()
            .borrow()
            .context
            .clone()
            .unwrap_or_else(|| current_context_name().to_owned());
        let mut title = format!(" Targets · {cluster} · {namespace} ");
        if loading {
            title.push_str(&format!("{} ", self.spinner()));
        }
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border))
            .title(Span::styled(
                title,
                Style::default()
                    .fg(theme::TEXT_EMPHASIS)
                    .bg(theme::FILL_HEAVY)
                    .bold(),
            ));

        let mut inner = block.inner(area);
        frame.render_widget(block, area);

        if self.filter_active || !self.filter.is_empty() {
            let filter_area = Rect { height: 1, ..inner };
            inner.y += 1;
            inner.height = inner.height.saturating_sub(1);
            let hits = match rows.len() {
                1 => "1 hit".to_owned(),
                count => format!("{count} hits"),
            };
            frame.render_widget(
                Line::from_iter([
                    Span::styled("/", Style::default().fg(theme::BRAND)),
                    Span::styled(
                        self.filter.clone(),
                        Style::default().fg(theme::TEXT_EMPHASIS),
                    ),
                    if self.filter_active {
                        Span::styled("█", Style::default().fg(theme::BRAND))
                    } else {
                        Span::raw("")
                    },
                    Span::styled(
                        format!("  {hits}"),
                        Style::default().fg(theme::TEXT_MUTED).italic(),
                    ),
                ]),
                filter_area,
            );
        }

        if rows.is_empty() {
            let message = if loading {
                format!(" {} loading targets…", self.spinner())
            } else if self.filter.is_empty() {
                " No targets found in this namespace.".to_owned()
            } else {
                " Nothing matches the filter.".to_owned()
            };
            frame.render_widget(
                Line::styled(message, Style::default().fg(theme::TEXT_MUTED)),
                inner,
            );
            return;
        }

        let items: Vec<ListItem> = rows
            .iter()
            .map(|row| row_item(row, inner.width as usize))
            .collect();
        let list = List::new(items).highlight_style(
            Style::default()
                .bg(theme::FILL_HEAVY)
                .fg(theme::TEXT_EMPHASIS)
                .bold(),
        );

        self.list_state.select(Some(self.selected));
        frame.render_stateful_widget(list, inner, &mut self.list_state);
    }
}

fn row_item(row: &Row, width: usize) -> ListItem<'static> {
    let line = match row {
        Row::Workload {
            kind,
            name,
            containers,
            expanded,
        } => {
            let badge = format!(" {} ", kind.badge());
            let suffix = (*containers > 0).then(|| {
                let marker = if *expanded { "▾" } else { "▸" };
                format!("  {marker} {containers} containers")
            });
            // The name yields to the badge and the container count, so a
            // long name never pushes them out of the pane.
            let name_budget = width
                .saturating_sub(badge.chars().count())
                .saturating_sub(suffix.as_deref().map_or(0, |s| s.chars().count()));
            let mut spans = vec![
                // A filled chip, so the kind column reads as a column.
                Span::styled(
                    badge,
                    Style::default()
                        .fg(theme::kind_color(*kind))
                        .bg(theme::FILL_HEAVY)
                        .bold(),
                ),
                Span::styled(
                    ellipsize(name, name_budget),
                    Style::default().fg(theme::TEXT_EMPHASIS),
                ),
            ];
            if let Some(suffix) = suffix {
                spans.push(Span::styled(suffix, Style::default().fg(theme::TEXT_MUTED)));
            }
            Line::from(spans)
        }
        Row::Container { name, last, .. } => {
            let prefix = format!("     {} ", if *last { "└─" } else { "├─" });
            let name_budget = width.saturating_sub(prefix.chars().count());
            Line::from_iter([
                Span::styled(prefix, Style::default().fg(theme::BORDER_DIM)),
                Span::styled(
                    ellipsize(name, name_budget),
                    Style::default().fg(theme::TEXT_EMPHASIS),
                ),
            ])
        }
        Row::Targetless => Line::from_iter([
            Span::styled(" ∅  ", Style::default().fg(theme::TEXT_MUTED)),
            Span::styled(
                "targetless",
                Style::default().fg(theme::TEXT_MUTED).italic(),
            ),
            Span::styled(
                "  (no target, cluster network only)",
                Style::default().fg(theme::TEXT_MUTED),
            ),
        ]),
        Row::KindError { kind, message } => {
            let badge = format!(" {} ", kind.badge());
            let message_budget = width.saturating_sub(badge.chars().count() + 13);
            Line::from_iter([
                Span::styled(badge, Style::default().fg(theme::TEXT_MUTED)),
                Span::styled(
                    format!("unavailable: {}", ellipsize(message, message_budget)),
                    Style::default().fg(theme::TEXT_MUTED).italic(),
                ),
            ])
        }
    };
    ListItem::new(line)
}

/// Case-insensitive subsequence match: every filter char appears in order.
fn subsequence_match(filter: &str, candidate: &str) -> bool {
    let mut chars = candidate.chars().flat_map(char::to_lowercase);
    filter
        .chars()
        .flat_map(char::to_lowercase)
        .all(|wanted| chars.any(|c| c == wanted))
}

/// Refetches targets whenever the client reconnects (which a namespace or
/// context switch triggers) or a manual refresh is requested.
async fn fetch_loop(mut context: Context, refresh: Arc<Notify>, data: Arc<RwLock<BrowserData>>) {
    loop {
        let client = context
            .client()
            .borrow_and_update()
            .as_ref()
            .and_then(|result| result.as_ref().ok())
            .cloned();

        if let Some(client) = client {
            let namespace = context
                .scope()
                .borrow()
                .namespace
                .clone()
                .unwrap_or_else(|| client.default_namespace().to_owned());

            let fetched = fetch(&client, &namespace).await;
            if let Ok(mut guard) = data.write() {
                *guard = fetched;
            }
            context.request_redraw();
        }

        let client_rx = context.client();
        tokio::select! {
            Ok(()) = client_rx.changed() => {},
            () = refresh.notified() => {},
        }
    }
}

async fn fetch(client: &kube::Client, namespace: &str) -> BrowserData {
    let kind_futures = TargetKind::VARIANTS.iter().map(|&kind| async move {
        let seeker = KubeResourceSeeker {
            client,
            namespace,
            copy_target: false,
        };
        let outcome = seeker
            .filtered(vec![kind.into()], true)
            .await
            .map(|paths| group_paths(kind, paths))
            .map_err(|error| error.to_string());
        KindListing { kind, outcome }
    });

    let kinds = join_all(kind_futures).await;

    BrowserData {
        namespace: namespace.to_owned(),
        kinds,
        loading: false,
    }
}

/// Groups seeker paths (`kind/name` or `kind/name/container/c`) into
/// workloads with their containers.
fn group_paths(kind: TargetKind, paths: Vec<String>) -> Vec<TargetItem> {
    let prefix = format!("{kind}/");
    let mut grouped: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for path in paths {
        let Some(rest) = path.strip_prefix(&prefix) else {
            continue;
        };
        match rest.split_once("/container/") {
            Some((name, container)) => grouped
                .entry(name.to_owned())
                .or_default()
                .push(container.to_owned()),
            None => {
                grouped.entry(rest.to_owned()).or_default();
            }
        }
    }

    grouped
        .into_iter()
        .map(|(name, containers)| TargetItem { name, containers })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_container_paths_under_their_workload() {
        let items = group_paths(
            TargetKind::Pod,
            vec![
                "pod/single".to_owned(),
                "pod/multi/container/app".to_owned(),
                "pod/multi/container/sidecar".to_owned(),
            ],
        );

        assert_eq!(items.len(), 2);
        assert_eq!(items[0].name, "multi");
        assert_eq!(items[0].containers, ["app", "sidecar"]);
        assert_eq!(items[1].name, "single");
        assert!(items[1].containers.is_empty());
    }

    #[test]
    fn subsequence_filter_matches_in_order_case_insensitively() {
        assert!(subsequence_match("", "anything"));
        assert!(subsequence_match("wapp", "Web-APP"));
        assert!(subsequence_match("sql", "cloud-sql-proxy"));
        assert!(!subsequence_match("xyz", "web-app"));
    }
}
