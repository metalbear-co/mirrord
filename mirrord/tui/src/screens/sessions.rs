use std::{
    ops::ControlFlow,
    sync::{Arc, RwLock},
    time::Duration,
};

use crossterm::event::Event;
use kube::{Api, api::ListParams};
use mirrord_operator::crd::{Session, SessionCrd};
use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    style::Style,
    text::Span,
    widgets::Cell,
};

use crate::{
    context::Context,
    screens::Screen,
    widgets::{
        pane::{Pane, PaneState},
        table::{Column, Table, TableRow},
    },
};

type Update = Arc<RwLock<Option<Result<Vec<SessionRow>, ()>>>>;

/// The sessions screen.
pub struct SessionsScreen {
    page_state: PaneState,
    update: Update,
    table: Table<SessionRow>,
    loaded: bool,
    failed: bool,
}

impl SessionsScreen {
    async fn run(mut context: Context, update: Update) {
        let mut interval = tokio::time::interval(Duration::from_secs(5));

        loop {
            tokio::select! {
                _ = interval.tick() => {}
                Ok(()) = context.client.changed() => {}
                Ok(()) = context.scope.changed() => {}
                Ok(()) = context.local_sessions.changed() => {}
                Ok(()) = context.local_only.changed() => {}
            };

            interval.reset();

            let client = match &*context.client.borrow_and_update() {
                Some(Ok(client)) => client.clone(),
                Some(Err(_)) => {
                    *update.write().unwrap() = Some(Err(()));

                    context.redraw.notify_one();

                    continue;
                }
                None => continue,
            };

            let namespace = context.scope.borrow_and_update().namespace.clone();

            let api: Api<SessionCrd> = match namespace {
                Some(namespace) => Api::namespaced(client.clone(), &namespace),
                None => Api::all(client.clone()),
            };

            // TODO: Filter list by local-only

            match api.list(&ListParams::default()).await {
                Ok(sessions) => {
                    *update.write().unwrap() = Some(Ok(sessions
                        .into_iter()
                        .map(|session| session.spec.session)
                        .filter(|session| !session.is_preview())
                        .map(SessionRow::new)
                        .collect()));

                    context.redraw.notify_one();
                }
                Err(error) => {
                    tracing::error!(
                        error = &error as &dyn std::error::Error,
                        "Failed to list sessions.",
                    );

                    *update.write().unwrap() = Some(Err(()));

                    context.redraw.notify_one();
                }
            }
        }
    }
}

impl Screen for SessionsScreen {
    fn new(context: Context) -> Self {
        let update = Update::default();

        tokio::spawn(Self::run(context, update.clone()));

        Self {
            page_state: PaneState::default(),
            update,
            table: Table::new("No active sessions."),
            loaded: false,
            failed: false,
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        if let Some(result) = self.update.write().unwrap().take() {
            self.loaded = true;

            match result {
                Ok(sessions) => {
                    self.failed = false;

                    self.table.set_rows(sessions);
                }
                Err(()) => self.failed = true,
            }
        }

        let page = Pane::new(!self.loaded, self.failed);

        let inner = page.inner(area);

        frame.render_stateful_widget(page, area, &mut self.page_state);

        if self.loaded {
            self.table.draw(frame, inner)
        } else {
            frame.render_widget(
                Span::styled("Loading...", Style::default().gray()).into_centered_line(),
                inner.centered(Constraint::Fill(1), Constraint::Length(1)),
            );
        }
    }

    fn handle_event(&mut self, event: Event) -> ControlFlow<(), Event> {
        self.table.handle_event(event)
    }
}

/// One row of the sessions table.
pub struct SessionRow {
    id: String,
    session: Session,
}

impl SessionRow {
    fn new(session: Session) -> Self {
        Self {
            id: session.id.clone().unwrap_or_default(),
            session,
        }
    }

    /// The session shown in this row.
    #[allow(unused, reason = "Nothing uses this yet.")]
    pub fn session(&self) -> &Session {
        &self.session
    }
}

impl TableRow for SessionRow {
    const COLUMNS: &'static [Column] = &[
        Column {
            name: "ID",
            width: Constraint::Length(18),
        },
        Column {
            name: "USER",
            width: Constraint::Fill(1),
        },
        Column {
            name: "TARGET",
            width: Constraint::Fill(2),
        },
        Column {
            name: "NAMESPACE",
            width: Constraint::Fill(1),
        },
        Column {
            name: "AGE",
            width: Constraint::Length(8),
        },
    ];

    type Id = String;

    fn id(&self) -> Self::Id {
        self.id.clone()
    }

    fn cells(&self) -> Vec<Cell<'_>> {
        vec![
            Cell::from(self.id.as_str()),
            Cell::from(self.session.user.as_str()),
            Cell::from(self.session.target.as_str()),
            Cell::from(self.session.namespace.as_deref().unwrap_or("-")),
            Cell::from(format_age(self.session.duration_secs)),
        ]
    }
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
