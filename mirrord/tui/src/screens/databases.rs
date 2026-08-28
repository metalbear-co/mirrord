use std::{
    io,
    ops::ControlFlow,
    sync::{Arc, RwLock},
    time::Duration,
};

use crossterm::event::{Event, KeyCode};
use k8s_openapi::jiff::{Timestamp, tz::TimeZone};
use kube::{Api, Client, Resource, ResourceExt};
use mirrord_operator::crd::db_branching::branch_database::{
    BranchDatabase, BranchDatabasePhase, ConnectionSource, ConnectionSourceKind, DatabaseDialect,
    MigrationsSpec, SessionInfo,
};
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

use crate::{context::Context, helpers::centered, screens::Screen, theme};

/// How often the list of branch databases is refreshed.
const REFRESH_INTERVAL: Duration = Duration::from_secs(5);

/// Placeholder used when a value is missing.
const DASH: &str = "-";

const ARROW_UP: char = '\u{2191}';
const ARROW_DOWN: char = '\u{2193}';
const BULLET: char = '\u{2022}';
const CURSOR: char = '\u{2588}';

/// The branch databases screen.
///
/// Shows every branch database the operator reports, one row per branch.
pub struct DatabasesScreen {
    data: Arc<RwLock<Data>>,
    table: TableState,
    /// Set while the user is looking at the details of a single branch.
    details: Option<Details>,
    /// The `/` search over the list.
    search: Search,
    refresh: Arc<Notify>,
}

impl DatabasesScreen {
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

            let client = current_client(&mut context);
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

    /// Lists the branch databases in the given namespace, or in all namespaces
    /// if no namespace is given.
    #[tracing::instrument(level = Level::DEBUG, skip(client), ret)]
    async fn fetch(client: Client, namespace: Option<&str>) -> State {
        let api: Api<BranchDatabase> = match namespace {
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
                tracing::debug!(
                    "Fetched {} {}",
                    list.items.len(),
                    BranchDatabase::plural(&()),
                )
            })
            .inspect_err(|error| {
                tracing::warn!(
                    %error,
                    "Failed to fetch {}",
                    BranchDatabase::plural(&()),
                );
            });
        match result {
            Ok(list) => {
                let mut branches = list.items;
                // Refreshes must not reshuffle the rows.
                branches.sort_by(|left, right| {
                    fn key(branch: &BranchDatabase) -> (&str, &str) {
                        (
                            branch.metadata.namespace.as_deref().unwrap_or_default(),
                            branch.metadata.name.as_deref().unwrap_or_default(),
                        )
                    }

                    key(left).cmp(&key(right))
                });
                State::Ready(branches)
            }
            Err(error) => State::Failed(error.to_string()),
        }
    }

    /// Draws the table of branches that match the search, or a message
    /// explaining why there is none.
    fn draw_body(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        state: &State,
        branches: &[&BranchDatabase],
    ) {
        if branches.is_empty() {
            let ready = if self.search.phrase.is_empty() {
                ("No active branch databases", theme::muted())
            } else {
                ("No branch databases match the search", theme::muted())
            };
            let (message, style) = state_message(state, ready);
            draw_centered_message(frame, area, message, style);
            return;
        }

        // The list shrinks as branches end, and as the search narrows it, so the selection can
        // outlive the row it pointed at.
        match self.table.selected() {
            Some(selected) if selected >= branches.len() => {
                self.table.select(Some(branches.len() - 1))
            }
            Some(_) => {}
            None => self.table.select(Some(0)),
        }

        frame.render_stateful_widget(
            Table::new(
                branches.iter().map(|branch| {
                    Row::new(Column::VARIANTS.iter().map(|column| {
                        let (value, style) = column.value(branch);
                        Cell::from(Span::styled(value, style))
                    }))
                }),
                Column::VARIANTS.iter().map(|column| column.constraint()),
            )
            .header(
                Row::new(Column::VARIANTS.iter().map(|column| column.title()))
                    .style(theme::table_header()),
            )
            .row_highlight_style(theme::selection()),
            area,
            &mut self.table,
        );
    }

    /// Enters the details of the selected branch.
    ///
    /// Does nothing when there is nothing to select.
    fn open_details(&mut self) {
        let data = self.data.clone();
        let data = data.read().unwrap();

        let State::Ready(branches) = &data.state else {
            return;
        };
        // The selection indexes the rows the user can see, not every branch.
        let branches = self.search.matching(branches);
        let Some(branch) = self
            .table
            .selected()
            .and_then(|selected| branches.get(selected))
        else {
            return;
        };
        let Some(name) = branch.metadata.name.clone() else {
            return;
        };

        self.details = Some(Details {
            namespace: branch.metadata.namespace.clone(),
            name,
            scroll: 0,
        });
    }

    /// Draws the details of the branch the user entered, or a message explaining
    /// why there are none.
    ///
    /// The branch is looked up in the data by name on every frame, so the
    /// details follow the refreshes.
    fn draw_details(&mut self, frame: &mut Frame, area: Rect, state: &State) {
        let Some(details) = &mut self.details else {
            return;
        };

        let branch = match state {
            State::Ready(branches) => branches.iter().find(|branch| {
                branch.metadata.name.as_deref() == Some(details.name.as_str())
                    && branch.metadata.namespace == details.namespace
            }),
            _ => None,
        };

        let Some(branch) = branch else {
            let (message, style) =
                state_message(state, ("This branch database has ended", theme::warning()));
            draw_centered_message(frame, area, message, style);
            return;
        };

        // The scrollbar keeps its column even when it is not drawn, so that the
        // text does not shift as the details grow past the panel.
        let [text_area, scrollbar_area] =
            Layout::horizontal([Constraint::Fill(1), Constraint::Length(1)]).areas(area);

        let lines = details_lines(branch);
        let max_scroll =
            u16::try_from(lines.len().saturating_sub(text_area.height.into())).unwrap_or(u16::MAX);
        // The details shrink as the branch progresses, so the scroll offset can
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

    /// The right-hand hint shown at the bottom of the panel.
    fn hint(&self) -> String {
        if self.details.is_some() {
            format!(
                "{ARROW_UP}/{ARROW_DOWN} scroll  {BULLET}  PgUp/PgDn page  {BULLET}  r refresh  {BULLET}  q back",
            )
        } else if self.search.editing {
            format!("type to filter  {BULLET}  Enter apply  {BULLET}  Esc clear")
        } else if self.search.phrase.is_empty() {
            format!(
                "{ARROW_UP}/{ARROW_DOWN} move  {BULLET}  Enter details  {BULLET}  / search  {BULLET}  r refresh",
            )
        } else {
            format!(
                "{ARROW_UP}/{ARROW_DOWN} move  {BULLET}  Enter details  {BULLET}  / search  {BULLET}  Esc clear  {BULLET}  r refresh",
            )
        }
    }

    /// The bordered panel that wraps the body, titled for the current mode.
    fn frame_block(&self, data: &Data, matching_len: usize) -> Block<'static> {
        let block = Block::bordered()
            .border_style(theme::border())
            .padding(Padding::horizontal(1));
        match &self.details {
            Some(details) => block
                .title(Span::styled(" Branch Database ", theme::title()))
                .title_top(
                    Line::styled(format!(" {} ", details.title()), theme::muted()).right_aligned(),
                ),
            None => block
                .title(Span::styled(" Branch Databases ", theme::title()))
                .title_top(
                    Line::styled(format!(" {} ", data.summary(matching_len)), theme::muted())
                        .right_aligned(),
                ),
        }
    }
}

impl Screen for DatabasesScreen {
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
            State::Ready(branches) => self.search.matching(branches),
            _ => Vec::new(),
        };

        let block = self.frame_block(&data, matching.len());
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
            Paragraph::new(Span::styled(self.hint(), theme::muted())).left_aligned(),
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
    /// `shown` is how many branches the search leaves in the list.
    fn summary(&self, shown: usize) -> String {
        match &self.state {
            State::Ready(branches) if shown < branches.len() => {
                format!("{shown}/{} branches", branches.len())
            }
            State::Ready(branches) if branches.len() == 1 => "1 branch".to_owned(),
            State::Ready(branches) => format!("{} branches", branches.len()),
            _ => String::new(),
        }
    }

    /// Returns a string that informs when [`Data::state`] was last replaced.
    fn updated(&self) -> String {
        let updated_at = self.updated_at.to_zoned(TimeZone::UTC);
        format!("updated at {updated_at:.0}")
    }
}

/// The outcome of the last attempt to list the branch databases.
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
    Ready(Vec<BranchDatabase>),
}

/// The `/` search that narrows the list of branches.
#[derive(Default)]
struct Search {
    /// The phrase the list is narrowed by. Empty means every branch is listed.
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

    /// Returns the branches that match [`Search::phrase`], in the order they
    /// were given.
    ///
    /// A branch matches when any one of the searched columns contains the
    /// phrase, ignoring case.
    fn matching<'a>(&self, branches: &'a [BranchDatabase]) -> Vec<&'a BranchDatabase> {
        if self.phrase.is_empty() {
            return branches.iter().collect();
        }

        let phrase = self.phrase.to_lowercase();

        branches
            .iter()
            .filter(|branch| {
                Column::SEARCHED.iter().any(|column| {
                    column
                        .value(branch)
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
            spans.push(Span::styled(CURSOR.to_string(), theme::title()));
        }

        frame.render_widget(Paragraph::new(Line::from_iter(spans)), area);
    }
}

/// The branch the user entered from the list, and how far they scrolled in it.
///
/// The branch is remembered by name, so that the details keep following it
/// across refreshes.
struct Details {
    /// Namespace of the branch, as in its metadata.
    namespace: Option<String>,
    /// Name of the branch.
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

/// Columns of the branch databases table.
///
/// The header and every row are built from this list, in this order.
#[derive(Clone, Copy, strum_macros::VariantArray)]
enum Column {
    Name,
    Namespace,
    Dialect,
    Target,
    Phase,
    Sessions,
    Age,
    Expires,
}

impl Column {
    /// The columns the `/` search matches the phrase against, each on its own.
    const SEARCHED: &'static [Self] = &[
        Self::Name,
        Self::Namespace,
        Self::Dialect,
        Self::Target,
        Self::Phase,
    ];

    /// The header cell of this column.
    fn title(self) -> &'static str {
        match self {
            Self::Name => "NAME",
            Self::Namespace => "NAMESPACE",
            Self::Dialect => "DIALECT",
            Self::Target => "TARGET",
            Self::Phase => "PHASE",
            Self::Sessions => "SESSIONS",
            Self::Age => "AGE",
            Self::Expires => "EXPIRES",
        }
    }

    /// The width of this column.
    fn constraint(self) -> Constraint {
        match self {
            Self::Name => Constraint::Fill(3),
            Self::Namespace => Constraint::Fill(2),
            Self::Dialect => Constraint::Length(11),
            Self::Target => Constraint::Fill(3),
            Self::Phase => Constraint::Length(8),
            Self::Sessions => Constraint::Length(8),
            Self::Age => Constraint::Length(8),
            Self::Expires => Constraint::Length(9),
        }
    }

    /// The text of this column for the given branch, and the style it is drawn
    /// with.
    fn value(self, branch: &BranchDatabase) -> (String, Style) {
        let status = branch.status.as_ref();

        match self {
            Self::Name => (branch.name_any(), Style::default()),
            Self::Namespace => (
                branch.namespace().unwrap_or_else(|| DASH.to_owned()),
                Style::default(),
            ),
            Self::Dialect => match dialect_name(branch) {
                Some(name) => (name, Style::default()),
                None => (DASH.to_owned(), theme::muted()),
            },
            Self::Target => {
                let target = &branch.spec.target;
                (format!("{}/{}", target.kind, target.name), Style::default())
            }
            Self::Phase => {
                let phase = status.map(|status| status.phase.clone());
                let text = phase
                    .as_ref()
                    .map(BranchDatabasePhase::to_string)
                    .unwrap_or_else(|| DASH.to_owned());

                (text, phase_style(phase.as_ref()))
            }
            Self::Sessions => match status {
                Some(status) if status.session_info.is_empty() => (DASH.to_owned(), theme::muted()),
                Some(status) => (status.session_info.len().to_string(), Style::default()),
                None => (DASH.to_owned(), theme::muted()),
            },
            Self::Age => match branch.creation_timestamp() {
                Some(created) => (
                    format_duration(Timestamp::now().duration_since(created.0).as_secs()),
                    Style::default(),
                ),
                None => (DASH.to_owned(), theme::muted()),
            },
            Self::Expires => match status {
                Some(status) => expires_countdown(status.expire_time.0, false),
                None => (DASH.to_owned(), theme::muted()),
            },
        }
    }
}

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

/// Style for the given phase of a branch.
fn phase_style(phase: Option<&BranchDatabasePhase>) -> Style {
    match phase {
        Some(BranchDatabasePhase::Ready) => Style::default().fg(theme::MINT),
        Some(BranchDatabasePhase::Init | BranchDatabasePhase::Pending) => {
            Style::default().fg(theme::AMBER)
        }
        Some(BranchDatabasePhase::Failed) => Style::default().fg(theme::CORAL),
        Some(BranchDatabasePhase::Unknown) | None => theme::muted(),
    }
}

/// Resolves the dialect of a branch to its display name, or `None` when the
/// spec has no (or more than one) dialect set.
fn dialect_name(branch: &BranchDatabase) -> Option<String> {
    branch
        .spec
        .dialect()
        .ok()
        .map(|config| DatabaseDialect::from(&config).to_string())
}

/// Builds every line of the details view of the given branch.
fn details_lines(branch: &BranchDatabase) -> Vec<Line<'static>> {
    let mut lines = overview_lines(branch);
    lines.extend(migrations_status_section(branch));
    lines.extend(target_section(branch));
    lines.extend(connection_source_section(branch));
    lines.extend(migrations_spec_section(branch));
    lines.extend(sessions_section(branch));
    lines
}

/// The top block: identity, phase, image, timestamps, TTL, expiry.
fn overview_lines(branch: &BranchDatabase) -> Vec<Line<'static>> {
    let spec = &branch.spec;
    let status = branch.status.as_ref();
    let phase = status.map(|status| status.phase.clone());

    let mut lines = vec![
        field(0, "Name", branch.name_any(), Style::default()),
        field(
            0,
            "Namespace",
            branch.namespace().unwrap_or_else(|| DASH.to_owned()),
            Style::default(),
        ),
        field(0, "ID", spec.id.clone(), Style::default()),
        field(
            0,
            "Dialect",
            dialect_name(branch).unwrap_or_else(|| DASH.to_owned()),
            Style::default(),
        ),
        field(
            0,
            "Database",
            spec.database_name
                .clone()
                .unwrap_or_else(|| DASH.to_owned()),
            Style::default(),
        ),
        field(
            0,
            "Phase",
            phase
                .as_ref()
                .map(BranchDatabasePhase::to_string)
                .unwrap_or_else(|| DASH.to_owned()),
            phase_style(phase.as_ref()),
        ),
    ];

    if let Some(error) = status.and_then(|status| status.error.as_deref()) {
        lines.push(field(0, "Error", error.to_owned(), theme::error()));
    }

    if let Some(pod_name) = status.and_then(|status| status.pod_name.as_deref()) {
        lines.push(field(0, "Pod", pod_name.to_owned(), Style::default()));
    }

    lines.push(field(
        0,
        "Image",
        match (spec.image.as_deref(), spec.version.as_deref()) {
            (Some(image), _) => image.to_owned(),
            (None, Some(version)) => format!("(default):{version}"),
            (None, None) => DASH.to_owned(),
        },
        Style::default(),
    ));

    match branch.creation_timestamp() {
        Some(created) => {
            let age = format_duration(Timestamp::now().duration_since(created.0).as_secs());
            let created = created.0.to_zoned(TimeZone::UTC);

            lines.push(field(
                0,
                "Created",
                format!("{created:.0} ({age} ago)"),
                Style::default(),
            ));
        }
        None => lines.push(field(0, "Created", DASH.to_owned(), theme::muted())),
    }

    lines.push(field(
        0,
        "TTL",
        format_duration(spec.ttl_secs as i64),
        Style::default(),
    ));

    if let Some(expire_time) = status.map(|status| &status.expire_time) {
        let (countdown, style) = expires_countdown(expire_time.0, true);
        let expire_time = expire_time.0.to_zoned(TimeZone::UTC);
        lines.push(field(
            0,
            "Expires",
            format!("{expire_time:.0} ({countdown})"),
            style,
        ));
    }

    lines
}

/// The `MIGRATIONS` section, taken from the status. Empty when no migration
/// has been observed.
fn migrations_status_section(branch: &BranchDatabase) -> Vec<Line<'static>> {
    let Some(migrations) = branch
        .status
        .as_ref()
        .and_then(|status| status.migrations.as_ref())
    else {
        return Vec::new();
    };

    let mut lines = vec![
        Line::default(),
        section("MIGRATIONS".to_owned()),
        field(
            2,
            "Phase",
            format!("{:?}", migrations.phase),
            Style::default(),
        ),
        field(
            2,
            "Observed generation",
            migrations.observed_generation.to_string(),
            Style::default(),
        ),
    ];
    if let Some(error) = migrations.error.as_deref() {
        lines.push(field(2, "Error", error.to_owned(), theme::error()));
    }
    lines
}

/// The `TARGET` section: the workload the branch attaches to.
fn target_section(branch: &BranchDatabase) -> Vec<Line<'static>> {
    let target = &branch.spec.target;
    let mut lines = vec![
        Line::default(),
        section("TARGET".to_owned()),
        field(2, "Kind", target.kind.clone(), Style::default()),
        field(2, "Name", target.name.clone(), Style::default()),
        field(
            2,
            "API version",
            target.api_version.clone(),
            Style::default(),
        ),
    ];
    match target.container.as_str() {
        "" => lines.push(field(2, "Container", DASH.to_owned(), theme::muted())),
        container => lines.push(field(
            2,
            "Container",
            container.to_owned(),
            Style::default(),
        )),
    }
    lines
}

/// The `CONNECTION SOURCE` section: where the branch reads its credentials.
fn connection_source_section(branch: &BranchDatabase) -> Vec<Line<'static>> {
    let mut lines = vec![Line::default(), section("CONNECTION SOURCE".to_owned())];
    match &branch.spec.connection_source {
        ConnectionSource::Url(url) => {
            lines.push(field(2, "Kind", "URL".to_owned(), Style::default()));
            for kind in url.iter() {
                lines.push(connection_source_line(kind));
            }
        }
        ConnectionSource::Params(params) => {
            lines.push(field(2, "Kind", "Parameters".to_owned(), Style::default()));
            for (label, source) in [
                ("host", params.host.as_ref()),
                ("port", params.port.as_ref()),
                ("user", params.user.as_ref()),
                ("password", params.password.as_ref()),
                ("database", params.database.as_ref()),
            ] {
                let Some(source) = source else {
                    continue;
                };
                for kind in source.iter() {
                    lines.push(param_line(label, kind));
                }
            }
            for (name, source) in &params.extra {
                for kind in source.iter() {
                    lines.push(param_line(name.as_str(), kind));
                }
            }
        }
    }
    lines
}

/// The `MIGRATIONS SPEC` section, taken from the spec. Empty when the branch
/// has no migrations configured.
fn migrations_spec_section(branch: &BranchDatabase) -> Vec<Line<'static>> {
    let Some(migrations) = branch.spec.migrations.as_ref() else {
        return Vec::new();
    };

    let mut lines = vec![Line::default(), section("MIGRATIONS SPEC".to_owned())];
    match migrations {
        MigrationsSpec::Flyway {
            image,
            archive,
            locations,
        } => {
            lines.push(field(2, "Flavor", "Flyway".to_owned(), Style::default()));
            if let Some(image) = image {
                lines.push(field(2, "Image", image.clone(), Style::default()));
            }
            lines.push(field(
                2,
                "Archive",
                if archive.is_some() {
                    "yes".to_owned()
                } else {
                    DASH.to_owned()
                },
                Style::default(),
            ));
            for location in locations {
                lines.push(field(2, "Location", location.clone(), Style::default()));
            }
        }
        MigrationsSpec::Container { image, .. } => {
            lines.push(field(2, "Flavor", "Container".to_owned(), Style::default()));
            lines.push(field(2, "Image", image.clone(), Style::default()));
        }
    }
    lines
}

/// The `SESSIONS` section: one row per open session, sorted by id.
fn sessions_section(branch: &BranchDatabase) -> Vec<Line<'static>> {
    // The map has no defined order; sort by id so refreshes do not shuffle.
    let mut sessions = branch
        .status
        .as_ref()
        .map(|status| status.session_info.values().collect::<Vec<_>>())
        .unwrap_or_default();
    sessions.sort_by(|a, b| a.id.cmp(&b.id));

    let mut lines = vec![
        Line::default(),
        section(format!("SESSIONS ({})", sessions.len())),
    ];
    if sessions.is_empty() {
        lines.push(Line::styled("  none", theme::muted()));
    }
    for SessionInfo { id, owner } in sessions {
        lines.push(Line::from_iter([
            Span::raw(format!("  {:<width$} ", id, width = LABEL_WIDTH - 2)),
            Span::styled(owner.to_string(), Style::default()),
        ]));
    }
    lines
}

/// Renders one [`ConnectionSourceKind`] as a details field line.
fn connection_source_line(kind: &ConnectionSourceKind) -> Line<'static> {
    let (label, value) = describe_source_kind(kind);
    field(2, label, value, Style::default())
}

/// Renders one named parameter [`ConnectionSourceKind`] as a details field line.
fn param_line(name: &str, kind: &ConnectionSourceKind) -> Line<'static> {
    let (label, value) = describe_source_kind(kind);
    field(2, name, format!("{label}: {value}"), Style::default())
}

/// A short human description of a [`ConnectionSourceKind`].
fn describe_source_kind(kind: &ConnectionSourceKind) -> (&'static str, String) {
    match kind {
        ConnectionSourceKind::Env {
            container,
            variable,
        } => (
            "env",
            match container {
                Some(container) => format!("{container}:${variable}"),
                None => format!("${variable}"),
            },
        ),
        ConnectionSourceKind::EnvFrom {
            container,
            variable,
        } => (
            "envFrom",
            match container {
                Some(container) => format!("{container}:${variable}"),
                None => format!("${variable}"),
            },
        ),
        ConnectionSourceKind::Secret { name, key, .. } => ("secret", format!("{name}/{key}")),
        ConnectionSourceKind::EnvPattern {
            container,
            variable,
            value_pattern,
        } => (
            "envPattern",
            match container {
                Some(container) => format!("{container}:${variable} ~ {value_pattern}"),
                None => format!("${variable} ~ {value_pattern}"),
            },
        ),
        ConnectionSourceKind::GcpSecretManager { secret_ref, .. } => {
            ("gcpSecretManager", secret_ref.clone())
        }
        ConnectionSourceKind::AwsSecretsManager { secret_ref, .. } => {
            ("awsSecretsManager", secret_ref.clone())
        }
    }
}

/// Formats a number of seconds the way `kubectl` formats an age.
fn format_duration(seconds: i64) -> String {
    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;

    let seconds = seconds.max(0);

    if seconds < MINUTE {
        format!("{seconds}s")
    } else if seconds < HOUR {
        format!("{}m{}s", seconds / MINUTE, seconds % MINUTE)
    } else if seconds < DAY {
        format!("{}h{}m", seconds / HOUR, seconds % HOUR / MINUTE)
    } else {
        format!("{}d{}h", seconds / DAY, seconds % DAY / HOUR)
    }
}

/// Text and style used to show how long remains until `expire_time`.
///
/// When still active, the duration is optionally prefixed with `in ` so it
/// reads naturally in prose (e.g. `"in 5m30s"`).
fn expires_countdown(expire_time: Timestamp, prefix_in: bool) -> (String, Style) {
    let remaining = expire_time.duration_since(Timestamp::now()).as_secs();
    if remaining <= 0 {
        return ("expired".to_owned(), Style::default().fg(theme::CORAL));
    }

    let duration = format_duration(remaining);
    let text = if prefix_in {
        format!("in {duration}")
    } else {
        duration
    };
    let style = if remaining < 60 {
        Style::default().fg(theme::AMBER)
    } else {
        Style::default()
    };
    (text, style)
}

/// Extracts the current [`Client`] from the context's watch channel, if any.
fn current_client(context: &mut Context) -> Option<Client> {
    context
        .client
        .borrow_and_update()
        .as_ref()
        .map(Result::as_ref)
        .transpose()
        .ok()
        .flatten()
        .cloned()
}

/// Picks the message and style shown when there is no branch to draw.
///
/// `ready` is used when [`State::Ready`] has nothing to show — it varies by
/// caller (empty list vs. missing selection vs. no search hits).
fn state_message<'a>(state: &'a State, ready: (&'a str, Style)) -> (&'a str, Style) {
    match state {
        State::Disconnected => ("Waiting for a connection to the cluster...", theme::muted()),
        State::Loading => ("Loading...", theme::muted()),
        State::Failed(error) => (error.as_str(), theme::error()),
        State::Ready(_) => ready,
    }
}

/// Draws a single line of text centered in the given area.
fn draw_centered_message(frame: &mut Frame, area: Rect, message: &str, style: Style) {
    frame.render_widget(
        Paragraph::new(Span::styled(message.to_owned(), style)).centered(),
        centered(area, Constraint::Fill(1), Constraint::Length(1)),
    );
}
