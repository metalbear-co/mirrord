use std::{
    ops::ControlFlow,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use crossterm::event::{Event, KeyCode, KeyModifiers};
use kube::{Api, Client, ResourceExt, api::ListParams};
use mirrord_operator::crd::{LockedPortCompat, SessionCrd};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::Style,
};
use strum::VariantArray;
use tokio::{
    sync::{Notify, watch},
    time::MissedTickBehavior,
};

use crate::{
    context::Context,
    kit::{
        confirm::{Answer, Confirm},
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

/// How often the list of sessions is refreshed.
const REFRESH_INTERVAL: Duration = Duration::from_secs(5);

/// How long the outcome of a kill stays on the panel.
const FLASH_FOR: Duration = Duration::from_secs(6);

/// The sessions screen.
///
/// Shows every mirrord session the operator reports, one row per session. Preview environment
/// sessions are left out; they have their own screen.
pub struct SessionsScreen {
    data: Arc<RwLock<Listing<SessionCrd>>>,
    client: watch::Receiver<Option<anyhow::Result<Client>>>,
    table: DataTable,
    /// The `/` search over the list.
    search: SearchBox,
    /// The session opened with `Enter`, shown whole.
    describe: Option<Describe>,
    /// The session `Ctrl+D` asked to kill, waiting for confirmation, by name.
    confirm: Option<(String, Confirm)>,
    /// What the last kill came to, and until when to show it.
    flash: Arc<RwLock<Option<(String, Style, Instant)>>>,
    refresh: Arc<Notify>,
}

impl SessionsScreen {
    /// Refreshes the list until the application exits.
    async fn run(
        mut context: Context,
        data: Arc<RwLock<Listing<SessionCrd>>>,
        refresh: Arc<Notify>,
    ) {
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

            let client = connected(&context.client.borrow_and_update());
            let namespace = context.scope.borrow_and_update().namespace.clone();

            let state = match client {
                Some(client) => {
                    let api: Api<SessionCrd> = match namespace {
                        Some(namespace) => Api::namespaced(client, &namespace),
                        None => Api::all(client),
                    };
                    fetch(api).await
                }
                None => Fetch::Disconnected,
            };

            data.write().unwrap().replace(state);
            context.redraw.notify_one();
        }
    }

    fn matching<'a>(&self, sessions: &'a [SessionCrd]) -> Vec<&'a SessionCrd> {
        sessions
            .iter()
            .filter(|session| {
                self.search
                    .matches_any(Column::ALL.iter().map(|column| column.value(session).0))
            })
            .collect()
    }

    /// The session under the cursor.
    fn selected(&self) -> Option<SessionCrd> {
        let data = self.data.read().unwrap();
        let Fetch::Ready(sessions) = &data.state else {
            return None;
        };
        let index = self.table.selected()?;
        self.matching(sessions)
            .get(index)
            .map(|session| (*session).clone())
    }

    /// Asks the operator to end the session `name`, the same request `mirrord session kill`
    /// sends, and reports the answer on the panel.
    fn kill(&self, name: String) {
        let Some(client) = connected(&self.client.borrow()) else {
            return;
        };
        let flash = self.flash.clone();
        let refresh = self.refresh.clone();
        tokio::spawn(async move {
            let api: Api<SessionCrd> = Api::all(client);
            let outcome = match api.delete(&name, &Default::default()).await {
                Ok(_) => (
                    format!("killed session {name}"),
                    Style::default().fg(theme::MINT),
                ),
                Err(error) => (format!("could not kill {name}: {error}"), theme::error()),
            };
            if let Ok(mut slot) = flash.write() {
                *slot = Some((outcome.0, outcome.1, Instant::now() + FLASH_FOR));
            }
            refresh.notify_one();
        });
    }
}

/// The client, when the last connection attempt succeeded.
fn connected(client: &Option<anyhow::Result<Client>>) -> Option<Client> {
    client
        .as_ref()
        .and_then(|result| result.as_ref().ok())
        .cloned()
}

/// Lists the sessions, sorted so that a refresh never reshuffles the rows under the selection.
async fn fetch(api: Api<SessionCrd>) -> Fetch<Vec<SessionCrd>> {
    match api.list(&ListParams::default()).await {
        Ok(list) => {
            let mut sessions: Vec<SessionCrd> = list
                .into_iter()
                .filter(|session| !session.spec.session.is_preview())
                .collect();
            sessions.sort_by(|left, right| {
                (left.spec.session.namespace.as_deref(), left.name_any())
                    .cmp(&(right.spec.session.namespace.as_deref(), right.name_any()))
            });
            Fetch::Ready(sessions)
        }
        Err(error) => {
            tracing::error!(
                error = &error as &dyn std::error::Error,
                "Failed to list sessions.",
            );
            Fetch::Failed(state_view::explain(&error, "sessions"))
        }
    }
}

impl Screen for SessionsScreen {
    fn new(context: Context) -> Self {
        let data = Arc::new(RwLock::new(Listing::default()));
        let refresh = Arc::new(Notify::new());
        let client = context.client.clone();

        tokio::spawn(Self::run(context, data.clone(), refresh.clone()));

        Self {
            data,
            client,
            table: DataTable::default(),
            search: SearchBox::default(),
            describe: None,
            confirm: None,
            flash: Arc::default(),
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
        let search_height = u16::from(self.search.visible());
        let [body, search] =
            Layout::vertical([Constraint::Min(3), Constraint::Length(search_height)]).areas(area);

        let matching = match &data.state {
            Fetch::Ready(sessions) => self.matching(sessions),
            _ => Vec::new(),
        };

        let flash = self.flash.read().ok().and_then(|flash| {
            flash
                .as_ref()
                .filter(|(_, _, until)| Instant::now() < *until)
                .map(|(message, style, _)| (message.clone(), *style))
        });
        let count = data.count(matching.len());
        let block = panel::block(
            "sessions",
            count.as_deref(),
            Some(&data.updated()),
            flash
                .as_ref()
                .map(|(message, style)| (message.as_str(), *style)),
        );
        let inner = block.inner(body);
        frame.render_widget(block, body);

        let empty = EmptyText {
            nothing: "No active sessions",
            no_match: "No sessions match the search",
        };
        let searching = !self.search.phrase().is_empty();
        if !state_view::draw(frame, inner, &data.state, matching.len(), searching, &empty) {
            self.table.draw::<Column>(frame, inner, &matching);
        }

        if search_height > 0 {
            self.search.draw(frame, search);
        }

        if let Some((_, confirm)) = &self.confirm {
            confirm.draw(frame, area);
        }
    }

    fn handle_event(&mut self, event: Event) -> ControlFlow<(), Event> {
        if let Some(describe) = &mut self.describe {
            if let DescribeOutcome::Closed = describe.handle_event(&event) {
                self.describe = None;
            }
            return ControlFlow::Break(());
        }

        if let Some((name, confirm)) = &self.confirm {
            if let Event::Key(key) = event {
                match confirm.handle_key(key) {
                    Answer::Pending => {}
                    Answer::Yes => {
                        let name = name.clone();
                        self.confirm = None;
                        self.kill(name);
                    }
                    Answer::No => self.confirm = None,
                }
            }
            return ControlFlow::Break(());
        }

        match self.search.handle_event(&event) {
            SearchOutcome::Ignored => {}
            SearchOutcome::Consumed => return ControlFlow::Break(()),
            SearchOutcome::Changed => {
                // The rows under the selection change with every keystroke.
                self.table.select_first();
                return ControlFlow::Break(());
            }
        }

        let Event::Key(key) = &event else {
            return ControlFlow::Continue(event);
        };

        if self.table.handle_key(key) {
            return ControlFlow::Break(());
        }

        match key.code {
            KeyCode::Enter => {
                if let Some(session) = self.selected() {
                    self.describe = Some(Describe::new(
                        format!("session {}", session.name_any()),
                        &session,
                    ));
                }
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(session) = self.selected() {
                    let name = session.name_any();
                    let details = &session.spec.session;
                    self.confirm = Some((
                        name.clone(),
                        Confirm::new(
                            format!("Kill session {name}?"),
                            vec![
                                format!("user     {}", details.user),
                                format!("target   {}", details.target),
                                "Its local process loses the cluster connection.".to_owned(),
                            ],
                        ),
                    ));
                }
            }
            KeyCode::Char('r') => self.refresh.notify_one(),
            _ => return ControlFlow::Continue(event),
        }

        ControlFlow::Break(())
    }

    fn hints(&self) -> Vec<Hint> {
        if let Some(describe) = &self.describe {
            return describe.hints();
        }
        if self.confirm.is_some() {
            return vec![Hint::new("enter", "kill"), Hint::new("esc", "cancel")];
        }
        if self.search.is_editing() {
            return self.search.hints();
        }

        let mut hints = vec![Hint::new("enter", "describe"), Hint::new("ctrl-d", "kill")];
        hints.extend(self.search.hints());
        hints.push(Hint::new("r", "refresh"));
        hints
    }
}

/// Columns of the sessions table. The `/` search matches every one of them.
#[derive(Clone, Copy, strum_macros::VariantArray)]
enum Column {
    Namespace,
    Id,
    User,
    Target,
    Ports,
    Key,
    Age,
}

impl Columns for Column {
    type Row = SessionCrd;

    const ALL: &'static [Self] = Self::VARIANTS;

    fn title(self) -> &'static str {
        match self {
            Self::Namespace => "NAMESPACE",
            Self::Id => "ID",
            Self::User => "USER",
            Self::Target => "TARGET",
            Self::Ports => "PORTS",
            Self::Key => "KEY",
            Self::Age => "AGE",
        }
    }

    fn constraint(self) -> Constraint {
        match self {
            Self::Namespace => Constraint::Fill(1),
            Self::Id => Constraint::Length(18),
            Self::User => Constraint::Fill(1),
            Self::Target => Constraint::Fill(2),
            Self::Ports => Constraint::Fill(1),
            Self::Key => Constraint::Fill(1),
            Self::Age => Constraint::Length(8),
        }
    }

    fn value(self, crd: &SessionCrd) -> (String, Style) {
        let session = &crd.spec.session;
        let muted = |text: String| (text, theme::muted());
        match self {
            Self::Namespace => muted(session.namespace.clone().unwrap_or_else(|| "-".to_owned())),
            Self::Id => (
                session.id.clone().unwrap_or_else(|| crd.name_any()),
                Style::default().fg(theme::LAVENDER),
            ),
            Self::User => (session.user.clone(), Style::default()),
            Self::Target => (
                session.target.clone(),
                Style::default().fg(theme::INDIGO_LIGHT),
            ),
            Self::Ports => match ports(session.locked_ports.as_deref()) {
                ports if ports.is_empty() => muted("-".to_owned()),
                // Ports a session holds are stolen; the operator only reports those.
                ports => (ports, theme::warning()),
            },
            Self::Key => muted(session.key.clone().unwrap_or_else(|| "-".to_owned())),
            Self::Age => muted(format_age(session.duration_secs)),
        }
    }
}

/// The ports a session holds, such as `80, 8080 (filtered)`.
fn ports(locked: Option<&[LockedPortCompat]>) -> String {
    locked
        .unwrap_or_default()
        .iter()
        .map(|port| {
            let (number, filter) = match port {
                LockedPortCompat::Modern(port) => (port.port, port.filter.is_some()),
                LockedPortCompat::Legacy(port, _, filter) => (*port, filter.is_some()),
            };
            if filter {
                format!("{number} (filtered)")
            } else {
                number.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Formats an elapsed number of seconds the way `kubectl` renders resource ages.
fn format_age(seconds: u64) -> String {
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds / 60),
        3600..86400 => format!("{}h{}m", seconds / 3600, seconds % 3600 / 60),
        _ => format!("{}d{}h", seconds / 86400, seconds % 86400 / 3600),
    }
}
