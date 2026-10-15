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
    widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState},
};
use strum::VariantArray;
use tokio::{sync::Notify, time::MissedTickBehavior};
use tracing::Level;

use crate::{
    context::Context,
    helpers::{centered, wrap},
    kit::{
        describe::{Describe, DescribeOutcome},
        hints::Hint,
        panel,
        search::{SearchBox, SearchOutcome},
        state_view::{self, EmptyText, Fetch, Listing},
        table::{Columns, DataTable},
    },
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
    table: DataTable,
    /// Set while the user is looking at the details of a single split.
    details: Option<Details>,
    /// The resource `y` opened, shown whole as YAML.
    describe: Option<Describe>,
    /// The `/` search over the list.
    search: SearchBox,
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
                data.replace(state);
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
            Err(error) => State::Failed(state_view::explain(&error, "queue splits")),
        }
    }

    /// Draws the table of splits that match the search, or a message explaining
    /// why there is none.
    fn draw_body(&mut self, frame: &mut Frame, area: Rect, state: &State, splits: &[&QueueSplit]) {
        let empty = EmptyText {
            nothing: "No active queue-splitting sessions",
            no_match: "No queue-splitting sessions match the search",
        };
        let searching = !self.search.phrase().is_empty();
        if state_view::draw(frame, area, state, splits.len(), searching, &empty) {
            return;
        }

        self.table.draw::<Column>(frame, area, splits);
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
        let splits = matching(&self.search, splits);
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

        let lines = details_lines(split, text_area.width.into());
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
            table: DataTable::default(),
            details: None,
            describe: None,
            search: SearchBox::default(),
            refresh,
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        if let Some(describe) = &mut self.describe {
            describe.draw(frame, area);
            return;
        }

        let data = self.data.clone();
        let data = data.read().unwrap();

        // The search box is only in the way while the user is not searching.
        let search_height = u16::from(self.search.visible() && self.details.is_none());
        let [body, search] =
            Layout::vertical([Constraint::Min(3), Constraint::Length(search_height)]).areas(area);

        let matching = match &data.state {
            State::Ready(splits) => matching(&self.search, splits),
            _ => Vec::new(),
        };

        // The details view takes over the whole panel, title included.
        let updated = data.updated();
        let block = match &self.details {
            Some(details) => panel::block(
                &format!("split({})", details.title()),
                None,
                Some(&updated),
                None,
            ),
            None => panel::block(
                "queues",
                data.count(matching.len()).as_deref(),
                Some(&updated),
                None,
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
    }

    fn handle_event(&mut self, event: Event) -> ControlFlow<(), Event> {
        if let Some(describe) = &mut self.describe {
            if let DescribeOutcome::Closed = describe.handle_event(&event) {
                self.describe = None;
            }
            return ControlFlow::Break(());
        }

        // `y` shows the split under the cursor, or the one whose details are open, as YAML.
        if let Event::Key(key) = &event
            && key.code == KeyCode::Char('y')
            && !self.search.is_editing()
            && let Some(resource) = self.focused_resource()
        {
            self.describe = Some(Describe::new(
                format!("split {}", resource.name_any()),
                &resource,
            ));
            return ControlFlow::Break(());
        }

        if self.details.is_none() {
            match self.search.handle_event(&event) {
                SearchOutcome::Ignored => {}
                SearchOutcome::Consumed => return ControlFlow::Break(()),
                SearchOutcome::Changed => {
                    // The rows under the selection change with every keystroke.
                    self.table.select_first();
                    return ControlFlow::Break(());
                }
            }
        }

        let Event::Key(key) = &event else {
            return ControlFlow::Continue(event);
        };

        if self.details.is_none() && self.table.handle_key(key) {
            return ControlFlow::Break(());
        }

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
            None => match key.code {
                KeyCode::Enter => self.open_details(),
                KeyCode::Char('r') => self.refresh.notify_one(),
                _ => return ControlFlow::Continue(event),
            },
        }

        ControlFlow::Break(())
    }

    fn hints(&self) -> Vec<Hint> {
        if let Some(describe) = &self.describe {
            return describe.hints();
        }
        if self.details.is_some() {
            return vec![
                Hint::new("\u{2191}/\u{2193}", "scroll"),
                Hint::new("y", "yaml"),
                Hint::new("pgup/pgdn", "page"),
                Hint::new("r", "refresh"),
                Hint::new("q", "back"),
            ];
        }
        if self.search.is_editing() {
            return self.search.hints();
        }

        let mut hints = DataTable::hints();
        hints.push(Hint::new("enter", "details"));
        hints.push(Hint::new("y", "yaml"));
        hints.extend(self.search.hints());
        hints.push(Hint::new("r", "refresh"));
        hints
    }
}

impl QueuesScreen {
    /// The split whose details are open, or else the one under the cursor.
    fn focused_resource(&self) -> Option<QueueSplit> {
        let data = self.data.read().unwrap();
        let State::Ready(splits) = &data.state else {
            return None;
        };
        let found = match &self.details {
            Some(details) => splits.iter().find(|item| {
                item.metadata.name.as_deref() == Some(details.name.as_str())
                    && item.metadata.namespace == details.namespace
            }),
            None => {
                let index = self.table.selected()?;
                matching(&self.search, splits).get(index).copied()
            }
        };
        found.cloned()
    }
}

/// Returns the splits that match the search, in the order they were given.
///
/// A split matches when any one of the searched columns contains the phrase.
fn matching<'a>(search: &SearchBox, splits: &'a [QueueSplit]) -> Vec<&'a QueueSplit> {
    splits
        .iter()
        .filter(|split| {
            search.matches_any(Column::SEARCHED.iter().map(|column| column.value(split).0))
        })
        .collect()
}

/// What the background task has found so far.
type Data = Listing<QueueSplit>;

/// The outcome of the last attempt to list the splits.
type State = Fetch<Vec<QueueSplit>>;

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
}

impl Columns for Column {
    type Row = QueueSplit;

    const ALL: &'static [Self] = Self::VARIANTS;

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

/// Width of the label column of the details view, indentation included.
const LABEL_WIDTH: usize = 20;

/// Indentation of a target pod's reason under the pod's line in the details view.
const REASON_INDENT: usize = 4;

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

/// Builds every line of the details view of the given split, `width` columns wide.
fn details_lines(split: &QueueSplit, width: usize) -> Vec<Line<'static>> {
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

        // A legacy map shows one attribute per line; a composed tree shows as one expression.
        if let Some(message_filter) = filter.message_filter() {
            match message_filter.as_attribute_map() {
                Some(attributes) => {
                    for (attribute, pattern) in attributes {
                        lines.push(field(4, &attribute, pattern, Style::default()));
                    }
                }
                None => lines.push(field(
                    4,
                    "filter",
                    message_filter.to_string(),
                    Style::default(),
                )),
            }
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
        if let Some(reason) = &pod.reason {
            lines.extend(
                wrap(reason, width.saturating_sub(REASON_INDENT))
                    .into_iter()
                    .map(|line| {
                        Line::styled(
                            format!("{blank:REASON_INDENT$}{line}", blank = ""),
                            theme::muted(),
                        )
                    }),
            );
        }
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
