use std::{
    io,
    ops::ControlFlow,
    sync::{Arc, RwLock},
    time::Duration,
};

use crossterm::event::{Event, KeyCode};
use k8s_openapi::jiff::{Timestamp, tz::TimeZone};
use kube::{Api, Client, Resource, ResourceExt};
use mirrord_operator::crd::{queue_split::QueueSplit, session::SessionTarget};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{
        Block, Cell, Padding, Paragraph, Row, Scrollbar, ScrollbarOrientation, ScrollbarState,
        Table, TableState,
    },
};
use strum::VariantArray;
use tokio::{sync::Notify, time::MissedTickBehavior};
use tracing::Level;

use crate::{
    context::Context,
    helpers::{centered, ellipsize},
    screens::Screen,
    theme,
};

/// How often the list of queue splits is refreshed.
const REFRESH_INTERVAL: Duration = Duration::from_secs(5);

/// The split queues screen.
///
/// Shows every queue-splitting session the operator reports, one row per split.
pub struct QueuesScreen {
    data: Arc<RwLock<Data>>,
    table: TableState,
    /// Set while the user is looking at the details of a single split.
    details: Option<Details>,
    /// The `/` search over the list.
    search: Search,
    refresh: Arc<Notify>,
}

impl QueuesScreen {
    /// Refreshes [`Data`] until the application exits.
    async fn run(mut context: Context, data: Arc<RwLock<Data>>, refresh: Arc<Notify>) {
        let mut refresh_interval = tokio::time::interval(REFRESH_INTERVAL);
        refresh_interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                changed = context.client.changed() => if changed.is_err() {
                    break;
                },
                changed = context.scope.changed() => if changed.is_err() {
                    break;
                },
                _ = refresh.notified() => {},
                _ = refresh_interval.tick() => {},
            }

            let client = context
                .client
                .borrow_and_update()
                .as_ref()
                .map(Result::as_ref)
                .transpose()
                .ok()
                .flatten()
                .cloned();
            let namespace = context.scope.borrow_and_update().namespace.clone();

            let state = match client {
                Some(client) => Self::fetch(client, namespace.as_deref()).await,
                None => State::Disconnected,
            };

            {
                let mut data = data.write().unwrap();
                data.state = state;
                data.updated_at = Timestamp::now();
            }

            context.redraw.notify_one();
        }
    }

    /// Lists the queue splits in the given namespace, or in all namespaces if
    /// no namespace is given.
    #[tracing::instrument(level = Level::DEBUG, skip(client), ret)]
    async fn fetch(client: Client, namespace: Option<&str>) -> State {
        let api: Api<QueueSplit> = match namespace {
            Some(namespace) => Api::namespaced(client, namespace),
            None => Api::all(client),
        };
        let result = tokio::time::timeout(Duration::from_secs(5), api.list(&Default::default()))
            .await
            .unwrap_or_else(|_elapsed| {
                Err(kube::Error::Service(Box::new(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "request did not finish within 5 seconds",
                ))))
            })
            .inspect(|list| {
                tracing::debug!("Fetched {} {}", list.items.len(), QueueSplit::plural(&()),)
            })
            .inspect_err(|error| {
                tracing::warn!(
                    %error,
                    "Failed to fetch {}",
                    QueueSplit::plural(&()),
                );
            });
        match result {
            Ok(list) => {
                let mut splits = list.items;
                // Refreshes must not reshuffle the rows.
                splits.sort_by(|left, right| {
                    fn key(split: &QueueSplit) -> (&str, &str) {
                        (
                            split.metadata.namespace.as_deref().unwrap_or_default(),
                            split.metadata.name.as_deref().unwrap_or_default(),
                        )
                    }

                    key(left).cmp(&key(right))
                });
                State::Ready(splits)
            }
            Err(error) => State::Failed(error.to_string()),
        }
    }

    /// Draws the table of splits that match the search, or a message explaining
    /// why there is none.
    fn draw_body(&mut self, frame: &mut Frame, area: Rect, state: &State, splits: &[&QueueSplit]) {
        if splits.is_empty() {
            let (message, style) = match state {
                State::Disconnected => {
                    ("Waiting for a connection to the cluster...", theme::muted())
                }
                State::Loading => ("Loading...", theme::muted()),
                State::Failed(error) => (error.as_str(), theme::error()),
                State::Ready(_) if !self.search.phrase.is_empty() => (
                    "No queue-splitting sessions match the search",
                    theme::muted(),
                ),
                State::Ready(_) => ("No active queue-splitting sessions", theme::muted()),
            };
            frame.render_widget(
                Paragraph::new(Span::styled(message, style)).centered(),
                centered(area, Constraint::Fill(1), Constraint::Length(1)),
            );
            return;
        }

        // The list shrinks as sessions end, and as the search narrows it, so the selection can
        // outlive the row it pointed at.
        match self.table.selected() {
            Some(selected) if selected >= splits.len() => self.table.select(Some(splits.len() - 1)),
            Some(_) => {}
            None => self.table.select(Some(0)),
        }

        // The table cuts whatever does not fit off without a trace, so the cells
        // are truncated here instead, against the very widths it lays out.
        let widths = Layout::horizontal(Column::VARIANTS.iter().map(|column| column.constraint()))
            .spacing(COLUMN_SPACING)
            .split(Rect { height: 1, ..area });
        let columns = || {
            Column::VARIANTS
                .iter()
                .zip(widths.iter().map(|rect| rect.width))
        };

        frame.render_stateful_widget(
            Table::new(
                splits.iter().map(|split| {
                    Row::new(columns().map(|(column, width)| column.cell(split, width)))
                }),
                Column::VARIANTS.iter().map(|column| column.constraint()),
            )
            .column_spacing(COLUMN_SPACING)
            .header(
                Row::new(columns().map(|(column, width)| ellipsize(column.title(), width.into())))
                    .style(theme::table_header()),
            )
            .row_highlight_style(theme::selection()),
            area,
            &mut self.table,
        );
    }

    /// Enters the details of the selected split.
    ///
    /// Does nothing when there is nothing to select.
    fn open_details(&mut self) {
        let data = self.data.clone();
        let data = data.read().unwrap();

        let State::Ready(splits) = &data.state else {
            return;
        };
        // The selection indexes the rows the user can see, not every split.
        let splits = self.search.matching(splits);
        let Some(split) = self
            .table
            .selected()
            .and_then(|selected| splits.get(selected))
        else {
            return;
        };
        let Some(name) = split.metadata.name.clone() else {
            return;
        };

        self.details = Some(Details {
            namespace: split.metadata.namespace.clone(),
            name,
            scroll: 0,
        });
    }

    /// Draws the details of the split the user entered, or a message explaining
    /// why there are none.
    ///
    /// The split is looked up in the data by name on every frame, so the details
    /// follow the refreshes.
    fn draw_details(&mut self, frame: &mut Frame, area: Rect, state: &State) {
        let Some(details) = &mut self.details else {
            return;
        };

        let split = match state {
            State::Ready(splits) => splits.iter().find(|split| {
                split.metadata.name.as_deref() == Some(details.name.as_str())
                    && split.metadata.namespace == details.namespace
            }),
            _ => None,
        };

        let Some(split) = split else {
            let (message, style) = match state {
                State::Disconnected => {
                    ("Waiting for a connection to the cluster...", theme::muted())
                }
                State::Loading => ("Loading...", theme::muted()),
                State::Failed(error) => (error.as_str(), theme::error()),
                State::Ready(_) => ("This queue-splitting session has ended", theme::warning()),
            };
            frame.render_widget(
                Paragraph::new(Span::styled(message, style)).centered(),
                centered(area, Constraint::Fill(1), Constraint::Length(1)),
            );
            return;
        };

        // The scrollbar keeps its column even when it is not drawn, so that the
        // text does not shift as the details grow past the panel.
        let [text_area, scrollbar_area] =
            Layout::horizontal([Constraint::Fill(1), Constraint::Length(1)]).areas(area);

        let lines = details_lines(split);
        let max_scroll =
            u16::try_from(lines.len().saturating_sub(text_area.height.into())).unwrap_or(u16::MAX);
        // The details shrink as the split progresses, so the scroll offset can
        // outlive the lines it pointed at.
        details.scroll = details.scroll.min(max_scroll);

        frame.render_widget(Paragraph::new(lines).scroll((details.scroll, 0)), text_area);

        if max_scroll > 0 {
            frame.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None)
                    .style(theme::border()),
                scrollbar_area,
                &mut ScrollbarState::new(usize::from(max_scroll) + 1)
                    .position(details.scroll.into())
                    .viewport_content_length(text_area.height.into()),
            );
        }
    }
}

impl Screen for QueuesScreen {
    fn new(context: Context) -> Self {
        let data = Arc::new(RwLock::new(Data::default()));
        let refresh = Arc::new(Notify::new());

        tokio::spawn(Self::run(context, data.clone(), refresh.clone()));

        Self {
            data,
            table: TableState::default(),
            details: None,
            search: Search::default(),
            refresh,
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let data = self.data.clone();
        let data = data.read().unwrap();

        // The search box is only in the way while the user is not searching.
        let search_height = u16::from(self.search.visible() && self.details.is_none());
        let [body, search, footer] = Layout::vertical([
            Constraint::Min(3),
            Constraint::Length(search_height),
            Constraint::Length(1),
        ])
        .areas(area);

        let matching = match &data.state {
            State::Ready(splits) => self.search.matching(splits),
            _ => Vec::new(),
        };

        let block = Block::bordered()
            .border_style(theme::border())
            .padding(Padding::horizontal(1));
        // The details view takes over the whole panel, title and hints included.
        let (block, hint) = match &self.details {
            Some(details) => (
                block
                    .title(Span::styled(" Queue Split ", theme::title()))
                    .title_top(
                        Line::styled(format!(" {} ", details.title()), theme::muted())
                            .right_aligned(),
                    ),
                "\u{2191}/\u{2193} scroll  \u{2022}  PgUp/PgDn page  \u{2022}  r refresh  \u{2022}  q back",
            ),
            None => (
                block
                    .title(Span::styled(" Queue Splits ", theme::title()))
                    .title_top(
                        Line::styled(
                            format!(" {} ", data.summary(matching.len())),
                            theme::muted(),
                        )
                        .right_aligned(),
                    ),
                if self.search.editing {
                    "type to filter  \u{2022}  Enter apply  \u{2022}  Esc clear"
                } else if self.search.phrase.is_empty() {
                    "\u{2191}/\u{2193} move  \u{2022}  Enter details  \u{2022}  / search  \u{2022}  r refresh"
                } else {
                    "\u{2191}/\u{2193} move  \u{2022}  Enter details  \u{2022}  / search  \u{2022}  Esc clear  \u{2022}  r refresh"
                },
            ),
        };
        let inner = block.inner(body);
        frame.render_widget(block, body);

        if self.details.is_some() {
            self.draw_details(frame, inner, &data.state);
        } else {
            self.draw_body(frame, inner, &data.state, &matching);
            if search_height > 0 {
                self.search.draw(frame, search);
            }
        }

        let [hints, updated] =
            Layout::horizontal([Constraint::Fill(1), Constraint::Fill(1)]).areas(footer);

        frame.render_widget(
            Paragraph::new(Span::styled(hint, theme::muted())).left_aligned(),
            hints,
        );
        frame.render_widget(
            Paragraph::new(Span::styled(data.updated(), theme::muted())).right_aligned(),
            updated,
        );
    }

    fn handle_event(&mut self, event: Event) -> ControlFlow<(), Event> {
        let Event::Key(key) = &event else {
            return ControlFlow::Continue(event);
        };

        match &mut self.details {
            Some(details) => match key.code {
                KeyCode::Down => details.scroll = details.scroll.saturating_add(1),
                KeyCode::Up => details.scroll = details.scroll.saturating_sub(1),
                KeyCode::PageDown => details.scroll = details.scroll.saturating_add(10),
                KeyCode::PageUp => details.scroll = details.scroll.saturating_sub(10),
                KeyCode::Home => details.scroll = 0,
                // Clamped to the length of the details when they are drawn.
                KeyCode::End => details.scroll = u16::MAX,
                KeyCode::Char('r') => self.refresh.notify_one(),
                KeyCode::Char('q') | KeyCode::Esc => self.details = None,
                _ => return ControlFlow::Continue(event),
            },
            // A modal text box: typing "q"/Tab/etc. here must not quit or
            // switch screens.
            None if self.search.editing => match key.code {
                KeyCode::Char(char) => {
                    self.search.phrase.push(char);
                    // The rows under the selection change with every keystroke.
                    self.table.select_first();
                }
                KeyCode::Backspace => {
                    self.search.phrase.pop();
                    self.table.select_first();
                }
                KeyCode::Enter => self.search.editing = false,
                KeyCode::Esc => {
                    self.search.editing = false;
                    self.search.phrase.clear();
                    self.table.select_first();
                }
                _ => {}
            },
            None => match key.code {
                KeyCode::Down => self.table.select_next(),
                KeyCode::Up => self.table.select_previous(),
                KeyCode::Home => self.table.select_first(),
                KeyCode::End => self.table.select_last(),
                KeyCode::PageDown => self.table.scroll_down_by(10),
                KeyCode::PageUp => self.table.scroll_up_by(10),
                KeyCode::Enter => self.open_details(),
                KeyCode::Char('/') => self.search.editing = true,
                KeyCode::Esc if !self.search.phrase.is_empty() => {
                    self.search.phrase.clear();
                    self.table.select_first();
                }
                KeyCode::Char('r') => self.refresh.notify_one(),
                _ => return ControlFlow::Continue(event),
            },
        }

        ControlFlow::Break(())
    }
}

/// What the background task has found so far.
#[derive(Default)]
struct Data {
    state: State,
    /// When [`Data::state`] was last replaced.
    updated_at: Timestamp,
}

impl Data {
    /// The right-hand side of the panel title.
    ///
    /// `shown` is how many splits the search leaves in the list.
    fn summary(&self, shown: usize) -> String {
        match &self.state {
            State::Ready(splits) if shown < splits.len() => {
                format!("{shown}/{} splits", splits.len())
            }
            State::Ready(splits) if splits.len() == 1 => "1 split".to_owned(),
            State::Ready(splits) => format!("{} splits", splits.len()),
            _ => String::new(),
        }
    }

    /// Returns a string that informs when [`Data::state`] was last replaced.
    fn updated(&self) -> String {
        let updated_at = self.updated_at.to_zoned(TimeZone::UTC);
        format!("updated at {updated_at:.0}")
    }
}

/// The outcome of the last attempt to list the queue splits.
#[derive(Default, Debug)]
enum State {
    /// The application is not connected to the cluster.
    Disconnected,
    /// The first attempt is still in flight.
    #[default]
    Loading,
    /// The attempt failed.
    Failed(String),
    /// The attempt succeeded.
    Ready(Vec<QueueSplit>),
}

/// The `/` search that narrows the list of splits.
#[derive(Default)]
struct Search {
    /// The phrase the list is narrowed by. Empty means every split is listed.
    phrase: String,
    /// Set while the user is typing the phrase.
    editing: bool,
}

impl Search {
    /// Whether the search box belongs on the screen.
    ///
    /// It stays up after the phrase is applied, so that the list is never
    /// silently narrowed.
    fn visible(&self) -> bool {
        self.editing || !self.phrase.is_empty()
    }

    /// Returns the splits that match [`Search::phrase`], in the order they were
    /// given.
    ///
    /// A split matches when any one of the searched columns contains the
    /// phrase, ignoring case.
    fn matching<'a>(&self, splits: &'a [QueueSplit]) -> Vec<&'a QueueSplit> {
        if self.phrase.is_empty() {
            return splits.iter().collect();
        }

        let phrase = self.phrase.to_lowercase();

        splits
            .iter()
            .filter(|split| {
                Column::SEARCHED.iter().any(|column| {
                    column
                        .value(split)
                        .0
                        .to_lowercase()
                        .contains(phrase.as_str())
                })
            })
            .collect()
    }

    /// Draws the search box.
    fn draw(&self, frame: &mut Frame, area: Rect) {
        let mut spans = vec![
            Span::styled("/", theme::title()),
            Span::raw(self.phrase.clone()),
        ];
        // The cursor only belongs in the box while it takes input.
        if self.editing {
            spans.push(Span::styled("\u{2588}", theme::title()));
        }

        frame.render_widget(Paragraph::new(Line::from_iter(spans)), area);
    }
}

/// The split the user entered from the list, and how far they scrolled in it.
///
/// The split is remembered by name, so that the details keep following it
/// across refreshes.
struct Details {
    /// Namespace of the split, as in its metadata.
    namespace: Option<String>,
    /// Name of the split.
    name: String,
    /// How many lines of the details are scrolled off the top.
    scroll: u16,
}

impl Details {
    /// The right-hand side of the panel title.
    fn title(&self) -> String {
        match &self.namespace {
            Some(namespace) => format!("{namespace}/{}", self.name),
            None => self.name.clone(),
        }
    }
}

/// Columns of the queue splits table.
///
/// The header and every row are built from this list, in this order.
#[derive(Clone, Copy, strum_macros::VariantArray)]
enum Column {
    Session,
    User,
    Namespace,
    Target,
    Phase,
    Queues,
    Pods,
    Duration,
}

impl Column {
    /// The columns the `/` search matches the phrase against, each on its own.
    const SEARCHED: &'static [Self] = &[
        Self::Session,
        Self::User,
        Self::Namespace,
        Self::Target,
        Self::Phase,
    ];

    /// The header cell of this column.
    fn title(self) -> &'static str {
        match self {
            Self::Session => "SESSION",
            Self::User => "USER",
            Self::Namespace => "NAMESPACE",
            Self::Target => "TARGET",
            Self::Phase => "PHASE",
            Self::Queues => "QUEUES",
            Self::Pods => "PODS",
            Self::Duration => "DURATION",
        }
    }

    /// The width of this column.
    fn constraint(self) -> Constraint {
        match self {
            Self::Session => Constraint::Length(16),
            Self::User => Constraint::Fill(4),
            Self::Namespace => Constraint::Fill(1),
            Self::Target => Constraint::Fill(3),
            Self::Phase => Constraint::Length(7),
            Self::Queues => Constraint::Length(6),
            Self::Pods => Constraint::Length(5),
            Self::Duration => Constraint::Length(8),
        }
    }

    /// The cell of this column for the given split, truncated to the width the
    /// table gives the column.
    fn cell(self, split: &QueueSplit, width: u16) -> Cell<'static> {
        let (value, style) = self.value(split);

        Cell::from(Span::styled(ellipsize(&value, width.into()), style))
    }

    /// The text of this column for the given split, and the style it is drawn
    /// with.
    fn value(self, split: &QueueSplit) -> (String, Style) {
        let spec = &split.spec;
        let status = split.status.as_ref();

        match self {
            Self::Session => (spec.session.clone(), Style::default()),
            Self::User => (spec.owner.to_string(), Style::default()),
            Self::Namespace => (split.namespace().unwrap_or_else(dash), Style::default()),
            Self::Target => (
                match &spec.target {
                    SessionTarget::KubeResource(target) => {
                        format!("{}/{}", target.kind, target.name)
                    }
                    target @ SessionTarget::PodSet(..) => target.display_name().into_owned(),
                },
                Style::default(),
            ),
            Self::Phase => {
                let phase = status.map(|status| status.phase.as_str());

                (phase.unwrap_or("-").to_owned(), phase_style(phase))
            }
            Self::Queues => match status {
                Some(status) => (status.queues.len().to_string(), Style::default()),
                None => (dash(), theme::muted()),
            },
            Self::Pods => {
                let pods = status
                    .map(|status| status.target_pods.as_slice())
                    .unwrap_or_default();

                if pods.is_empty() {
                    (dash(), theme::muted())
                } else {
                    // A pod only takes part in the split once it has been
                    // patched and has come back up.
                    let live = pods.iter().filter(|pod| pod.patched && pod.ready).count();

                    (
                        format!("{live}/{}", pods.len()),
                        if live == pods.len() {
                            Style::default().fg(theme::MINT)
                        } else {
                            Style::default().fg(theme::AMBER)
                        },
                    )
                }
            }
            Self::Duration => match split.creation_timestamp() {
                Some(created) => (
                    humantime::format_duration(Duration::from_secs(
                        u64::try_from(Timestamp::now().duration_since(created.0).as_secs())
                            .unwrap_or_default(),
                    ))
                    .to_string(),
                    Style::default(),
                ),
                None => (dash(), theme::muted()),
            },
        }
    }
}

/// Space the table leaves between two columns.
///
/// Set on the table so that the widths used for truncation are the widths the
/// table lays out.
const COLUMN_SPACING: u16 = 1;

/// Width of the label column of the details view, indentation included.
const LABEL_WIDTH: usize = 20;

/// Builds one `<label> <value>` line of the details view.
fn field(indent: usize, label: &str, value: impl Into<String>, style: Style) -> Line<'static> {
    let label = format!("{blank:indent$}{label}", blank = "");

    Line::from_iter([
        Span::styled(format!("{label:<LABEL_WIDTH$} "), theme::muted()),
        Span::styled(value.into(), style),
    ])
}

/// Builds the heading of one section of the details view.
fn section(title: String) -> Line<'static> {
    Line::styled(title, theme::table_header())
}

/// Style for the given phase of a split.
fn phase_style(phase: Option<&str>) -> Style {
    match phase {
        Some("Ready") => Style::default().fg(theme::MINT),
        Some("Pending") => Style::default().fg(theme::AMBER),
        Some("Failed") => Style::default().fg(theme::CORAL),
        _ => theme::muted(),
    }
}

/// Builds every line of the details view of the given split.
fn details_lines(split: &QueueSplit) -> Vec<Line<'static>> {
    let spec = &split.spec;
    let status = split.status.as_ref();
    let phase = status.map(|status| status.phase.as_str());

    let mut lines = vec![
        field(0, "Name", split.name_any(), Style::default()),
        field(
            0,
            "Namespace",
            split.namespace().unwrap_or_else(dash),
            Style::default(),
        ),
        field(0, "Session", spec.session.clone(), Style::default()),
        field(
            0,
            "Phase",
            phase.unwrap_or("-").to_owned(),
            phase_style(phase),
        ),
    ];

    if let Some(message) = status.and_then(|status| status.message.as_deref()) {
        lines.push(field(0, "Message", message.to_owned(), phase_style(phase)));
    }

    match split.creation_timestamp() {
        Some(created) => {
            let age = Duration::from_secs(
                u64::try_from(Timestamp::now().duration_since(created.0).as_secs())
                    .unwrap_or_default(),
            );
            let age = humantime::format_duration(age);
            let created = created.0.to_zoned(TimeZone::UTC);

            lines.push(field(
                0,
                "Created",
                format!("{created:.0} ({age} ago)"),
                Style::default(),
            ));
        }
        None => lines.push(field(0, "Created", dash(), theme::muted())),
    }

    let owner = &spec.owner;
    lines.extend([
        Line::default(),
        section("OWNER".to_owned()),
        field(2, "Username", owner.username.clone(), Style::default()),
        field(
            2,
            "Kubernetes user",
            owner.k8s_username.clone(),
            Style::default(),
        ),
        field(2, "Hostname", owner.hostname.clone(), Style::default()),
        field(2, "User ID", owner.user_id.clone(), Style::default()),
    ]);

    lines.extend([Line::default(), section("TARGET".to_owned())]);
    match &spec.target {
        SessionTarget::KubeResource(target) => lines.extend([
            field(2, "Kind", target.kind.clone(), Style::default()),
            field(2, "Name", target.name.clone(), Style::default()),
            field(
                2,
                "API version",
                target.api_version.clone(),
                Style::default(),
            ),
        ]),
        SessionTarget::PodSet(target) => lines.extend([
            field(2, "Kind", "PodSet", Style::default()),
            field(
                2,
                "Label selector",
                target.label_selector().to_string(),
                Style::default(),
            ),
        ]),
    }
    match spec.target.container() {
        "" => lines.push(field(2, "Container", dash(), theme::muted())),
        container => lines.push(field(
            2,
            "Container",
            container.to_owned(),
            Style::default(),
        )),
    }

    let queues = status
        .map(|status| status.queues.as_slice())
        .unwrap_or_default();
    lines.extend([
        Line::default(),
        section(format!("QUEUES ({})", queues.len())),
    ]);
    if queues.is_empty() {
        lines.push(Line::styled("  none", theme::muted()));
    }
    for queue in queues {
        lines.push(entry(&queue.id, &queue.queue_type));

        // Which of these the operator resolves depends on the broker type.
        for (label, value) in [
            ("Queue", queue.queue.as_deref()),
            ("Topic", queue.topic.as_deref()),
            ("Consumer group", queue.consumer_group.as_deref()),
            ("Subscription", queue.subscription.as_deref()),
        ] {
            if let Some(value) = value {
                lines.push(field(4, label, value.to_owned(), Style::default()));
            }
        }
    }

    lines.extend([
        Line::default(),
        section(format!("FILTERS ({})", spec.filters.len())),
    ]);
    if spec.filters.is_empty() {
        lines.push(Line::styled("  none", theme::muted()));
    }
    for filter in &spec.filters {
        lines.push(entry(&filter.id, &filter.queue_type));

        for (attribute, pattern) in &filter.message_filter {
            lines.push(field(4, attribute, pattern.clone(), Style::default()));
        }
        if let Some(jq_filter) = &filter.jq_filter {
            lines.push(field(4, "jq", jq_filter.clone(), Style::default()));
        }
    }

    let pods = status
        .map(|status| status.target_pods.as_slice())
        .unwrap_or_default();
    // A pod only takes part in the split once it has been patched and has come
    // back up.
    let live = pods.iter().filter(|pod| pod.patched && pod.ready).count();
    lines.extend([
        Line::default(),
        section(format!("TARGET PODS ({live}/{})", pods.len())),
    ]);
    if pods.is_empty() {
        lines.push(Line::styled("  none", theme::muted()));
    }
    for pod in pods {
        let (state, style) = match (pod.patched, pod.ready) {
            (true, true) => ("patched, ready", Style::default().fg(theme::MINT)),
            (true, false) => ("patched, not ready", Style::default().fg(theme::AMBER)),
            (false, true) => ("not patched, ready", Style::default().fg(theme::AMBER)),
            (false, false) => ("not patched, not ready", Style::default().fg(theme::AMBER)),
        };

        lines.push(Line::from_iter([
            Span::raw(format!("  {:<width$} ", pod.name, width = LABEL_WIDTH - 2)),
            Span::styled(state, style),
        ]));
    }

    lines
}

/// Builds the heading of one queue or one filter of the details view.
fn entry(id: &str, queue_type: &str) -> Line<'static> {
    Line::from_iter([
        Span::raw(format!("  {id}")),
        Span::styled(format!(" ({queue_type})"), theme::muted()),
    ])
}

/// Placeholder used when a value is missing.
fn dash() -> String {
    "-".to_owned()
}
