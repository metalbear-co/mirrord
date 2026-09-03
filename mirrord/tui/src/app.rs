use std::{ops::ControlFlow, sync::Arc, time::Duration};

use anyhow::Context as _;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;
use kube::Client;
use ratatui::{
    DefaultTerminal, Frame,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use strum::{EnumCount, VariantArray};
use tokio::{
    sync::{Notify, watch},
    task::AbortHandle,
    time::MissedTickBehavior,
};

use crate::{
    context::Context,
    local_sessions::LocalSessions,
    scope::Scope,
    screens::{
        Screen, databases::DatabasesScreen, home::HomeScreen, preview_envs::PreviewEnvsScreen,
        queues::QueuesScreen, sessions::SessionsScreen, targets::TargetsScreen,
        terminal::TerminalScreen,
    },
    status, theme,
    widgets::picker::{Picker, PickerOutcome},
};

/// The label the namespace picker uses for "no override" - the cluster's
/// default namespace.
const DEFAULT_NAMESPACE: &str = "(default)";

/// An app-level picker overlay, reachable from every screen.
enum AppPicker {
    Context(Picker),
    Namespace(Picker),
}

/// The running application.
pub struct App {
    home_state: HomeScreen,
    targets_state: TargetsScreen,
    sessions_state: SessionsScreen,
    databases_state: DatabasesScreen,
    queues_state: QueuesScreen,
    preview_envs_state: PreviewEnvsScreen,
    terminal_state: TerminalScreen,
    active: ActiveScreen,
    quit: bool,
    scope: watch::Sender<Scope>,
    client: watch::Sender<Option<anyhow::Result<Client>>>,
    #[expect(unused, reason = "Not yet implemented.")]
    local_sessions: watch::Sender<Option<LocalSessions>>,
    #[expect(unused, reason = "Not yet implemented.")]
    local_only: watch::Sender<bool>,
    redraw: Arc<Notify>,
    abort_connect: Option<AbortHandle>,
    /// A context or namespace picker floating over whatever screen is
    /// active.
    picker: Option<AppPicker>,
    /// Namespace names fetched for the picker, handed over on the next
    /// draw.
    fetched_namespaces: Arc<std::sync::RwLock<Option<Vec<String>>>>,
    /// Whether `e` has opened the connection error dialog. Only ever true while the last
    /// connection attempt failed - `connect` closes it again.
    error_details: bool,
}

impl App {
    /// Builds the application.
    pub fn new() -> Self {
        let scope = watch::Sender::new(Scope::default());
        let client = watch::Sender::new(None);
        let local_sessions = watch::Sender::new(None);
        let local_only = watch::Sender::new(false);

        let redraw = Arc::new(Notify::new());

        let context = Context::new(
            scope.subscribe(),
            client.subscribe(),
            local_sessions.subscribe(),
            local_only.subscribe(),
            redraw.clone(),
        );

        Self {
            home_state: HomeScreen::new(context.clone()),
            targets_state: TargetsScreen::new(context.clone()),
            sessions_state: SessionsScreen::new(context.clone()),
            databases_state: DatabasesScreen::new(context.clone()),
            queues_state: QueuesScreen::new(context.clone()),
            preview_envs_state: PreviewEnvsScreen::new(context.clone()),
            terminal_state: TerminalScreen::new(context),
            active: ActiveScreen::Home,
            quit: false,
            scope,
            client,
            local_sessions,
            local_only,
            redraw,
            abort_connect: None,
            picker: None,
            fetched_namespaces: Arc::default(),
            error_details: false,
        }
    }

    fn open_context_picker(&mut self) {
        let Ok(config) = kube::config::Kubeconfig::read() else {
            return;
        };
        let contexts: Vec<String> = config
            .contexts
            .iter()
            .map(|context| context.name.clone())
            .collect();
        if contexts.is_empty() {
            return;
        }
        let active = self
            .scope
            .borrow()
            .context
            .clone()
            .or(config.current_context);
        self.picker = Some(AppPicker::Context(Picker::new(
            "Kube Context",
            contexts,
            active,
        )));
    }

    fn open_namespace_picker(&mut self) {
        let client = match &*self.client.borrow() {
            Some(Ok(client)) => client.clone(),
            _ => return,
        };
        let active = self
            .scope
            .borrow()
            .namespace
            .clone()
            .unwrap_or_else(|| DEFAULT_NAMESPACE.to_owned());
        self.picker = Some(AppPicker::Namespace(Picker::loading(
            "Namespace",
            Some(active),
        )));

        let store = self.fetched_namespaces.clone();
        let redraw = self.redraw.clone();
        tokio::spawn(async move {
            let names = kube::Api::<k8s_openapi::api::core::v1::Namespace>::all(client)
                .list(&Default::default())
                .await
                .map(|list| {
                    list.items
                        .into_iter()
                        .filter_map(|namespace| namespace.metadata.name)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if let Ok(mut slot) = store.write() {
                *slot = Some(names);
            }
            redraw.notify_one();
        });
    }

    fn connect(&mut self, scope: &Scope) {
        if let Some(abort_handle) = self.abort_connect.take() {
            abort_handle.abort();
        }

        // Show "connecting…" while the swap is in flight, and drop a dialog describing a failure
        // that is no longer the current one.
        _ = self.client.send_replace(None);
        self.error_details = false;

        let scope = scope.clone();
        let client = self.client.clone();

        let handle = tokio::spawn(async move {
            let result = async {
                let built = scope.build_client().await?;
                // Building a client never touches the network; prove the
                // cluster answers before calling it connected.
                built
                    .apiserver_version()
                    .await
                    .context("cluster unreachable")?;
                Ok(built)
            }
            .await;

            if let Err(error) = &result {
                // The status bar and its dialog only have room for a summary of this; the log is
                // where the whole thing has to survive.
                tracing::error!("Failed to connect to the cluster: {error:#}");
            }

            _ = client.send_replace(Some(result));
        });

        self.abort_connect = Some(handle.abort_handle());
    }

    /// Runs until the user quits.
    pub async fn run(&mut self, terminal: &mut DefaultTerminal) -> anyhow::Result<()> {
        let mut events = EventStream::new();

        let mut scope = self.scope.subscribe();
        let mut client = self.client.subscribe();

        let mut ticker = tokio::time::interval(Duration::from_millis(250));

        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

        scope.mark_changed();

        loop {
            terminal.draw(|frame| self.draw(frame))?;

            tokio::select! {
                _ = ticker.tick() => {}
                Some(event) = events.next() => {
                    match event {
                        Ok(event) => self.handle_event(event),
                        Err(error) => tracing::error!(
                            error = &error as &dyn std::error::Error,
                            "Failed to receive terminal event.",
                        ),
                    }
                },
                Ok(()) = scope.changed() => self.connect(&scope.borrow_and_update()),
                Ok(()) = client.changed() => client.mark_unchanged(),
                () = self.redraw.notified() => {}
            };

            if self.quit {
                break;
            }
        }

        Ok(())
    }

    /// The two-line header: a logo chip, padded tab chips, and a rule
    /// whose brand-colored segment underlines the active tab.
    fn draw_header(&self, frame: &mut Frame, area: Rect) {
        const LOGO: &str = " mirrord ";
        const GAP: &str = "  ";
        /// Left gutter so the header doesn't sit glued to the corner.
        const GUTTER: &str = " ";

        let mut top: Vec<Span> = vec![
            Span::raw(GUTTER),
            Span::styled(
                LOGO,
                Style::default()
                    .fg(theme::LAVENDER)
                    .bg(theme::INDIGO)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(GAP),
        ];
        let mut cursor = GUTTER.chars().count() + LOGO.chars().count() + GAP.chars().count();
        let mut active = (0usize, 0usize);

        for (index, variant) in ActiveScreen::VARIANTS.iter().enumerate() {
            let label = match variant {
                ActiveScreen::Home => "Home",
                ActiveScreen::Targets => "Targets",
                ActiveScreen::Sessions => "Sessions",
                ActiveScreen::Databases => "Databases",
                ActiveScreen::Queues => "Queues",
                ActiveScreen::PreviewEnvs => "Preview Environments",
                ActiveScreen::Terminal => "Terminal",
            };
            let padded = format!("  {label}  ");
            let padded_width = padded.chars().count();
            if index == self.active as usize {
                active = (cursor, padded_width);
                top.push(Span::styled(
                    padded,
                    Style::default()
                        .fg(theme::LAVENDER)
                        .bg(theme::DEEP)
                        .add_modifier(Modifier::BOLD),
                ));
            } else {
                top.push(Span::styled(padded, theme::muted()));
            }
            cursor += padded_width;
            top.push(Span::raw(" "));
            cursor += 1;
        }

        // A discoverability nudge, right-aligned when there is room.
        const SWITCH_HINT: &str = "⇥ switch ";
        let width = area.width as usize;
        if width > cursor + SWITCH_HINT.chars().count() + 2 {
            top.push(Span::raw(
                " ".repeat(width - cursor - SWITCH_HINT.chars().count()),
            ));
            top.push(Span::styled(SWITCH_HINT, theme::muted()));
        }

        let (start, length) = active;
        let start = start.min(width);
        let length = length.min(width - start);
        let underline = Line::from_iter([
            Span::styled("─".repeat(start), Style::default().fg(theme::DEEP)),
            Span::styled("─".repeat(length), Style::default().fg(theme::INDIGO)),
            Span::styled(
                "─".repeat(width.saturating_sub(start + length)),
                Style::default().fg(theme::DEEP),
            ),
        ]);

        // A breathing row above the chips keeps the header off the
        // terminal's top edge.
        frame.render_widget(
            Paragraph::new(vec![Line::raw(""), Line::from_iter(top), underline]),
            area,
        );
    }

    fn draw(&mut self, frame: &mut Frame) {
        let [tabs_area, body, status_area] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .areas(frame.area());

        self.draw_header(frame, tabs_area);

        // Hand freshly fetched namespaces to a waiting picker.
        if let Some(AppPicker::Namespace(picker)) = &mut self.picker
            && picker.is_loading()
            && let Ok(mut slot) = self.fetched_namespaces.write()
            && let Some(mut names) = slot.take()
        {
            names.sort();
            let mut items = vec![DEFAULT_NAMESPACE.to_owned()];
            items.extend(names);
            picker.set_items(items);
        }

        match self.active {
            ActiveScreen::Home => self.home_state.draw(frame, body),
            ActiveScreen::Targets => self.targets_state.draw(frame, body),
            ActiveScreen::Sessions => self.sessions_state.draw(frame, body),
            ActiveScreen::Databases => self.databases_state.draw(frame, body),
            ActiveScreen::Queues => self.queues_state.draw(frame, body),
            ActiveScreen::PreviewEnvs => self.preview_envs_state.draw(frame, body),
            ActiveScreen::Terminal => self.terminal_state.draw(frame, body),
        }

        let scope = self.scope.borrow();
        let client = self.client.borrow();

        status::draw(frame, status_area, &scope, &client);

        // What the status bar had to cut short, on request. Never over a picker, which owns the
        // keyboard while it is open and would leave this dialog undismissable.
        if let (true, Some(Err(error)), None) = (self.error_details, &*client, &self.picker) {
            status::draw_details(frame, body, error);
        }

        if let Some(AppPicker::Context(picker)) | Some(AppPicker::Namespace(picker)) = &self.picker
        {
            picker.draw(frame, body);
        }
    }

    fn handle_event(&mut self, event: Event) {
        // What the terminal actually delivers - the ground truth when a
        // key combo "does nothing" (terminals eat or re-encode many).
        tracing::debug!(?event, "terminal event");

        // Key releases only show up under the kitty protocol, which stays off, and on Windows;
        // forwarding them would fire every binding twice and double every keystroke the terminal
        // screen writes to its pty.
        if let Event::Key(key) = &event
            && !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
        {
            return;
        }

        // An open picker owns the keyboard until it closes.
        if self.picker.is_some() {
            let Event::Key(key) = event else { return };
            let outcome = match &mut self.picker {
                Some(AppPicker::Context(picker)) | Some(AppPicker::Namespace(picker)) => {
                    picker.handle_key(key)
                }
                None => return,
            };
            match outcome {
                PickerOutcome::Open => {}
                PickerOutcome::Cancelled => self.picker = None,
                PickerOutcome::Picked(choice) => match self.picker.take() {
                    Some(AppPicker::Context(_)) => {
                        // A new cluster invalidates the namespace override.
                        _ = self.scope.send(Scope {
                            context: Some(choice),
                            namespace: None,
                        });
                    }
                    Some(AppPicker::Namespace(_)) => {
                        self.scope.send_modify(|scope| {
                            scope.namespace = (choice != DEFAULT_NAMESPACE).then_some(choice);
                        });
                    }
                    None => {}
                },
            }
            return;
        }

        // The error dialog is modal too: while it is up it takes every key, so that the screen
        // underneath cannot act on one the user aimed at the dialog.
        if self.error_details {
            let Event::Key(key) = event else { return };
            if matches!(
                key.code,
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('e')
            ) {
                self.error_details = false;
            }
            return;
        }

        let event = match self.active {
            ActiveScreen::Home => self.home_state.handle_event(event),
            ActiveScreen::Targets => self.targets_state.handle_event(event),
            ActiveScreen::Sessions => self.sessions_state.handle_event(event),
            ActiveScreen::Databases => self.databases_state.handle_event(event),
            ActiveScreen::Queues => self.queues_state.handle_event(event),
            ActiveScreen::PreviewEnvs => self.preview_envs_state.handle_event(event),
            ActiveScreen::Terminal => self.terminal_state.handle_event(event),
        };

        let ControlFlow::Continue(Event::Key(event)) = event else {
            return;
        };

        match event.code {
            KeyCode::Char('q') => {
                self.quit = true;
            }
            KeyCode::Char('c') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.quit = true;
            }
            // Only worth opening over a failure - with nothing to explain, `e` is free for the
            // screens to use.
            KeyCode::Char('e') => {
                self.error_details = matches!(&*self.client.borrow(), Some(Err(_)));
            }
            // Scope pickers work from every screen (when the screen didn't
            // claim the key for itself).
            KeyCode::Char('c') => self.open_context_picker(),
            KeyCode::Char('n') => self.open_namespace_picker(),
            KeyCode::Tab => {
                let current = self.active as usize;
                let count = ActiveScreen::COUNT;
                self.active = ActiveScreen::from_repr((current + 1) % count).unwrap();
            }
            KeyCode::BackTab => {
                let current = self.active as usize;
                let count = ActiveScreen::COUNT;
                self.active = ActiveScreen::from_repr((current + count - 1) % count).unwrap();
            }
            _ => {}
        }
    }
}

#[derive(
    Clone, Copy, strum_macros::FromRepr, strum_macros::EnumCount, strum_macros::VariantArray,
)]
#[repr(usize)]
enum ActiveScreen {
    Home,
    Targets,
    Sessions,
    Databases,
    Queues,
    PreviewEnvs,
    Terminal,
}
