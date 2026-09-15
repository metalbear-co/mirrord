use std::{
    ops::ControlFlow,
    sync::{Arc, RwLock},
};

use crossterm::event::{Event, KeyCode, KeyEventKind};
use kube::{Api, Client, api::DeleteParams, runtime::reflector::Store};
use mirrord_operator::crd::preview::PreviewSession;
use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    style::Style,
    text::Span,
    widgets::Paragraph,
};
use tokio::sync::watch;

use crate::{context::Context, helpers::centered, screens::Screen, telemetry::Telemetry};

mod data;
mod ui;
mod view;

use data::PreviewEnvsTree;
use ui::{Mode, StopCandidate, UiState};

/// The preview environments screen: every `PreviewSession` in the cluster, grouped by
/// namespace then target, with keyboard navigation, collapsible groups, and a `/`-triggered
/// substring search over each preview's key.
pub struct PreviewEnvsScreen {
    data: Arc<RwLock<Option<anyhow::Result<Store<PreviewSession>>>>>,
    /// Its own handle on the connection, independent of `data`'s background watch task —
    /// stopping a session is a one-off write triggered synchronously from `handle_event`,
    /// not something that belongs in the long-running watch loop.
    client: watch::Receiver<Option<anyhow::Result<Client>>>,
    ui: UiState,
    telemetry: Telemetry,
}

impl PreviewEnvsScreen {
    /// Reads the current data, flattens it under the current UI state, and applies `f` to the
    /// resulting rows. A no-op while still connecting or on a connection error.
    fn with_rows(&mut self, f: impl FnOnce(&mut UiState, &[view::Row])) {
        let guard = self.data.read().unwrap();
        let Some(Ok(store)) = &*guard else {
            return;
        };
        let tree = PreviewEnvsTree::build(store.state());
        drop(guard);

        let rows = view::flatten(&tree, &self.ui);
        f(&mut self.ui, &rows);
    }

    /// Fires off the actual deletes for a confirmed `Mode::ConfirmStop`, one per candidate.
    /// Fire-and-forget: the background watch (`data::run`) will reflect the result — success
    /// or the session simply staying put — as it naturally comes in over the watch stream, so
    /// there's nothing further for this call site to wait on. Failures are logged rather than
    /// surfaced in the UI; a session that fails to delete just stays visible, which is itself
    /// an adequate (if not maximally informative) signal that something needs another look.
    fn stop(&self, candidates: Vec<StopCandidate>) {
        let client = {
            let snapshot = self.client.borrow();
            match &*snapshot {
                Some(Ok(client)) => client.clone(),
                _ => return,
            }
        };

        self.telemetry.preview_envs_stopped(candidates.len() as u32);

        tokio::spawn(async move {
            for candidate in candidates {
                let api: Api<PreviewSession> =
                    Api::namespaced(client.clone(), &candidate.namespace);
                if let Err(error) = api.delete(&candidate.name, &DeleteParams::default()).await {
                    tracing::warn!(
                        error = &error as &dyn std::error::Error,
                        namespace = %candidate.namespace,
                        name = %candidate.name,
                        "failed to stop preview session",
                    );
                }
            }
        });
    }
}

impl Screen for PreviewEnvsScreen {
    fn new(context: Context) -> Self {
        let data = Arc::new(RwLock::new(None));
        let client = context.client.clone();
        let telemetry = context.telemetry.clone();

        tokio::spawn(data::run(context, data.clone()));

        Self {
            data,
            client,
            ui: UiState::default(),
            telemetry,
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let guard = self.data.read().unwrap();
        match &*guard {
            None => draw_message(frame, area, "connecting...", false),
            Some(Err(error)) => draw_message(frame, area, &error.to_string(), true),
            Some(Ok(store)) => {
                let tree = PreviewEnvsTree::build(store.state());
                drop(guard);
                view::draw(frame, area, &tree, &mut self.ui);
            }
        }
    }

    fn handle_event(&mut self, event: Event) -> ControlFlow<(), Event> {
        let Event::Key(key) = &event else {
            return ControlFlow::Continue(event);
        };
        if key.kind != KeyEventKind::Press {
            return ControlFlow::Continue(event);
        }

        // `Mode` isn't `Copy` (the `GoTo` variant owns a `String`/`Selection`), so matching it
        // by value has to take ownership rather than just copy it out. `mem::take` leaves
        // `Mode::default()` (`Browsing`) behind temporarily; every arm below puts a concrete
        // value back into `self.ui.mode` before returning.
        match std::mem::take(&mut self.ui.mode) {
            // While the help overlay is open it captures all input (only Esc does anything),
            // so it's actually "in focus" rather than something you could accidentally
            // navigate or filter behind.
            Mode::Help => {
                self.ui.mode = if key.code == KeyCode::Esc {
                    Mode::Browsing
                } else {
                    Mode::Help
                };
                ControlFlow::Break(())
            }
            // A destructive action, so it's confirmed rather than fired immediately from `s`
            // in `Browsing` — see `UiState::request_stop`. A bulk scope (target/namespace)
            // additionally requires typing its `stop_confirmation_word` before `Enter` does
            // anything, so bulk-stopping can't happen from a single reflexive keystroke the
            // way stopping one env can. Everything except Enter/Esc (and, for a bulk scope,
            // typing toward that word) is a no-op, same as every other modal state here.
            Mode::ConfirmStop {
                scope,
                candidates,
                mut confirmation,
            } => {
                let required_word = scope.stop_confirmation_word();
                match key.code {
                    KeyCode::Char(c) if required_word.is_some() => {
                        confirmation.push(c);
                        self.ui.mode = Mode::ConfirmStop {
                            scope,
                            candidates,
                            confirmation,
                        };
                    }
                    KeyCode::Backspace if required_word.is_some() => {
                        confirmation.pop();
                        self.ui.mode = Mode::ConfirmStop {
                            scope,
                            candidates,
                            confirmation,
                        };
                    }
                    KeyCode::Enter if required_word.is_none_or(|word| confirmation == word) => {
                        self.stop(candidates);
                        self.ui.mode = Mode::Browsing;
                    }
                    KeyCode::Esc => self.ui.mode = Mode::Browsing,
                    _ => {
                        self.ui.mode = Mode::ConfirmStop {
                            scope,
                            candidates,
                            confirmation,
                        };
                    }
                }
                ControlFlow::Break(())
            }
            Mode::Filtering => {
                match key.code {
                    // Live: the list is filtered on `ui.filter` directly, on every keystroke.
                    KeyCode::Char(c) => {
                        self.ui.filter.push(c);
                        self.ui.mode = Mode::Filtering;
                    }
                    KeyCode::Backspace => {
                        self.ui.filter.pop();
                        self.ui.mode = Mode::Filtering;
                    }
                    // Enter locks in whatever is already typed and returns to navigation.
                    KeyCode::Enter => self.ui.mode = Mode::Browsing,
                    // Esc clears the filter entirely and returns to navigation.
                    KeyCode::Esc => {
                        self.ui.filter.clear();
                        self.ui.mode = Mode::Browsing;
                    }
                    _ => self.ui.mode = Mode::Filtering,
                }
                // A modal text box: typing "q"/Tab/etc. here must not quit or switch screens.
                ControlFlow::Break(())
            }
            Mode::GoTo {
                mut input,
                origin,
                found,
            } => {
                match key.code {
                    KeyCode::Char(c) => {
                        input.push(c);
                        self.ui.mode = Mode::GoTo {
                            input,
                            origin,
                            found,
                        };
                        self.with_rows(|ui, rows| ui.update_goto_search(rows));
                    }
                    KeyCode::Backspace => {
                        input.pop();
                        self.ui.mode = Mode::GoTo {
                            input,
                            origin,
                            found,
                        };
                        self.with_rows(|ui, rows| ui.update_goto_search(rows));
                    }
                    // Enter locks in wherever the search landed and returns to navigation.
                    KeyCode::Enter => self.ui.mode = Mode::Browsing,
                    // Esc restores the exact row focused before `g` was pressed.
                    KeyCode::Esc => {
                        self.ui.selection = Some(origin);
                        self.ui.mode = Mode::Browsing;
                    }
                    _ => {
                        self.ui.mode = Mode::GoTo {
                            input,
                            origin,
                            found,
                        };
                    }
                }
                ControlFlow::Break(())
            }
            Mode::Browsing => {
                self.ui.mode = Mode::Browsing;
                match key.code {
                    KeyCode::Char('/') => {
                        self.ui.mode = Mode::Filtering;
                        ControlFlow::Break(())
                    }
                    KeyCode::Char('?') => {
                        self.ui.mode = Mode::Help;
                        ControlFlow::Break(())
                    }
                    KeyCode::Char('g') => {
                        if let Some(origin) = self.ui.selection.clone() {
                            self.ui.mode = Mode::GoTo {
                                input: String::new(),
                                origin,
                                found: true,
                            };
                        }
                        ControlFlow::Break(())
                    }
                    KeyCode::Char('s') => {
                        self.with_rows(|ui, rows| ui.request_stop(rows));
                        ControlFlow::Break(())
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        self.with_rows(|ui, rows| ui.move_cursor(rows, -1));
                        ControlFlow::Break(())
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        self.with_rows(|ui, rows| ui.move_cursor(rows, 1));
                        ControlFlow::Break(())
                    }
                    KeyCode::Left | KeyCode::Char('h') => {
                        self.with_rows(|ui, rows| ui.collapse_or_up(rows));
                        ControlFlow::Break(())
                    }
                    KeyCode::Right | KeyCode::Char('l') => {
                        self.with_rows(|ui, rows| ui.expand_or_down(rows));
                        ControlFlow::Break(())
                    }
                    KeyCode::Char('n') => {
                        self.with_rows(|ui, rows| ui.next_namespace(rows));
                        ControlFlow::Break(())
                    }
                    KeyCode::Char('N') => {
                        self.with_rows(|ui, rows| ui.prev_namespace(rows));
                        ControlFlow::Break(())
                    }
                    KeyCode::Char('t') => {
                        self.with_rows(|ui, rows| ui.next_target(rows));
                        ControlFlow::Break(())
                    }
                    KeyCode::Char('T') => {
                        self.with_rows(|ui, rows| ui.prev_target(rows));
                        ControlFlow::Break(())
                    }
                    KeyCode::Char('e') => {
                        self.with_rows(|ui, rows| ui.next_env(rows));
                        ControlFlow::Break(())
                    }
                    KeyCode::Char('E') => {
                        self.with_rows(|ui, rows| ui.prev_env(rows));
                        ControlFlow::Break(())
                    }
                    KeyCode::Esc if !self.ui.filter.is_empty() => {
                        self.ui.filter.clear();
                        ControlFlow::Break(())
                    }
                    // Toggles the focused env card's detail view. Navigating no longer changes
                    // card size on its own — only an explicit Enter does.
                    KeyCode::Enter => {
                        self.with_rows(|ui, rows| ui.toggle_expanded(rows));
                        ControlFlow::Break(())
                    }
                    _ => ControlFlow::Continue(event),
                }
            }
        }
    }
}

fn draw_message(frame: &mut Frame, area: Rect, message: &str, is_error: bool) {
    let style = if is_error {
        Style::default().red()
    } else {
        Style::default().gray()
    };
    frame.render_widget(
        Paragraph::new(Span::styled(message, style)).centered(),
        centered(area, Constraint::Fill(1), Constraint::Length(1)),
    );
}
