use std::{ops::ControlFlow, sync::Arc, time::Duration};

use anyhow::Context as _;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;
use kube::Client;
use ratatui::{
    DefaultTerminal, Frame,
    layout::{Constraint, Layout, Rect},
    style::Style,
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
    helpers::{centered, dialog},
    kit::{
        command_bar::{CommandBar, Entry, Outcome as CommandOutcome},
        hints::{self, Hint},
    },
    local_sessions::LocalSessions,
    scope::Scope,
    screens::{
        Screen, config::ConfigScreen, databases::DatabasesScreen, home::HomeScreen,
        preview_envs::PreviewEnvsScreen, queues::QueuesScreen, resources::ResourcesScreen,
        run::RunScreen, sessions::SessionsScreen, targets::TargetsScreen, terminal::TerminalScreen,
    },
    sidebar::{Click, Item, Sidebar, Switch},
    status,
    telemetry::Telemetry,
    theme,
    widgets::picker::{Picker, PickerOutcome},
};

/// What `:ns` takes for "every namespace", the same as picking no namespace.
const ALL_NAMESPACES: &str = "all";

/// The label the namespace picker uses for "no override" - the cluster's
/// default namespace.
const DEFAULT_NAMESPACE: &str = "(default)";

/// The unsaved edits quitting would lose, and why saving them failed, if it did.
struct QuitConfirm {
    unsaved: Vec<String>,
    error: Option<String>,
}

/// Keys that work on every screen the active screen does not claim for itself, listed in the
/// `?` help dialog.
const GLOBAL_HINTS: &[Hint] = &[
    Hint::new(":", "go to a view: :sessions, :ns dev"),
    Hint::new("Ctrl+P", "command palette"),
    Hint::new("Tab / Shift+Tab", "next / previous screen"),
    Hint::new("c", "switch kube context"),
    Hint::new("n", "switch namespace"),
    Hint::new("m", "only my sessions / everything"),
    Hint::new("e", "connection error details"),
    Hint::new("?", "this help"),
    Hint::new("q", "quit"),
];

/// An app-level picker overlay, reachable from every screen.
enum AppPicker {
    Context(Picker),
    Namespace(Picker),
    Command(Picker),
}

/// Something the command palette can do. Each one is also reachable by a key, so the palette is
/// where users find an action whose key they do not know yet.
#[derive(Clone, Copy)]
enum Command {
    GoTo(ActiveScreen),
    /// The resource type at this index of the Resources view's discovered types.
    Resource(usize),
    SwitchContext,
    SwitchNamespace,
    ShowError,
    ShowHelp,
    Quit,
}

impl Command {
    fn all() -> impl Iterator<Item = Self> {
        ActiveScreen::VARIANTS
            .iter()
            .copied()
            .map(Self::GoTo)
            .chain([
                Self::SwitchContext,
                Self::SwitchNamespace,
                Self::ShowError,
                Self::ShowHelp,
                Self::Quit,
            ])
    }

    /// The palette entry, with the key that does the same thing so users learn it.
    fn label(self) -> String {
        match self {
            Self::GoTo(screen) => format!("Go to {}", screen.label()),
            Self::Resource(index) => format!("Resource type {index}"),
            Self::SwitchContext => "Switch kube context  (c)".to_owned(),
            Self::SwitchNamespace => "Switch namespace  (n)".to_owned(),
            Self::ShowError => "Show connection error  (e)".to_owned(),
            Self::ShowHelp => "Show keys for this screen  (?)".to_owned(),
            Self::Quit => "Quit  (q)".to_owned(),
        }
    }
}

/// The running application.
pub struct App {
    home_state: HomeScreen,
    run_state: RunScreen,
    targets_state: TargetsScreen,
    config_state: ConfigScreen,
    resources_state: ResourcesScreen,
    sessions_state: SessionsScreen,
    databases_state: DatabasesScreen,
    queues_state: QueuesScreen,
    preview_envs_state: PreviewEnvsScreen,
    terminal_state: TerminalScreen,
    active: ActiveScreen,
    quit: bool,
    scope: watch::Sender<Scope>,
    client: watch::Sender<Option<anyhow::Result<Client>>>,
    /// "Only my sessions" (`m`): whether screens narrow to what this machine's sessions made.
    local_only: watch::Sender<bool>,
    /// The sessions running here, for the sidebar's switch.
    local_sessions: watch::Receiver<Option<LocalSessions>>,
    redraw: Arc<Notify>,
    abort_connect: Option<AbortHandle>,
    /// A context or namespace picker floating over whatever screen is
    /// active.
    picker: Option<AppPicker>,
    /// The `:` command line, while it is open.
    command_bar: Option<CommandBar<Command>>,
    sidebar: Sidebar,
    /// Namespaces of the connected cluster, offered by `:ns`. Filled once per connection.
    known_namespaces: Arc<std::sync::RwLock<Vec<String>>>,
    /// Namespace names fetched for the picker, handed over on the next
    /// draw.
    fetched_namespaces: Arc<std::sync::RwLock<Option<Vec<String>>>>,
    /// Whether `e` has opened the connection error dialog. Only ever true while the last
    /// connection attempt failed - `connect` closes it again.
    error_details: bool,
    /// Whether `?` has opened the help dialog.
    help: bool,
    /// Set while asking whether to quit with unsaved edits.
    quit_confirm: Option<QuitConfirm>,
    telemetry: Telemetry,
}

impl App {
    /// Builds the application.
    pub fn new(telemetry: Telemetry) -> Self {
        let scope = watch::Sender::new(Scope::default());
        let client = watch::Sender::new(None);
        let local_sessions = watch::Sender::new(None);
        let local_only = watch::Sender::new(false);

        let redraw = Arc::new(Notify::new());
        // The registry watch owns the sender; screens follow it through the context.
        let local_sessions_rx = local_sessions.subscribe();
        tokio::spawn(LocalSessions::watch(local_sessions, redraw.clone()));

        let context = Context::new(
            scope.subscribe(),
            client.subscribe(),
            local_sessions_rx.clone(),
            local_only.subscribe(),
            redraw.clone(),
            telemetry.clone(),
        );

        let mut app = Self {
            home_state: HomeScreen::new(context.clone()),
            run_state: RunScreen::new(context.clone()),
            targets_state: TargetsScreen::new(context.clone()),
            config_state: ConfigScreen::new(context.clone()),
            resources_state: ResourcesScreen::new(context.clone()),
            sessions_state: SessionsScreen::new(context.clone()),
            databases_state: DatabasesScreen::new(context.clone()),
            queues_state: QueuesScreen::new(context.clone()),
            preview_envs_state: PreviewEnvsScreen::new(context.clone()),
            terminal_state: TerminalScreen::new(context),
            active: ActiveScreen::Home,
            quit: false,
            scope,
            client,
            local_only,
            local_sessions: local_sessions_rx,
            redraw,
            abort_connect: None,
            picker: None,
            fetched_namespaces: Arc::default(),
            command_bar: None,
            sidebar: Sidebar::default(),
            known_namespaces: Arc::default(),
            error_details: false,
            help: false,
            quit_confirm: None,
            telemetry,
        };

        // Inside a project the TUI opens where a session is started; anywhere else, on the
        // dashboard of what is already running.
        if app.run_state.in_project() {
            app.active = ActiveScreen::Run;
        }
        app
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

    fn open_command_palette(&mut self) {
        let labels = Command::all().map(Command::label).collect();
        self.picker = Some(AppPicker::Command(Picker::new("Command", labels, None)));
    }

    fn open_command_bar(&mut self) {
        let mut entries: Vec<Entry<Command>> = ActiveScreen::VARIANTS
            .iter()
            .map(|screen| Entry {
                names: screen
                    .names()
                    .iter()
                    .map(|name| (*name).to_owned())
                    .collect(),
                about: screen.about(),
                action: Command::GoTo(*screen),
                args: Vec::new(),
            })
            .collect();

        // Every resource type the cluster serves, by its plural and its kind, so
        // `:mirrordsplitconfigs` and `:mirrordsplitconfig` both open it.
        entries.extend(
            self.resources_state
                .kinds()
                .iter()
                .enumerate()
                .map(|(index, kind)| Entry {
                    names: vec![kind.plural().to_owned(), kind.resource.kind.to_lowercase()],
                    about: "mirrord resource",
                    action: Command::Resource(index),
                    args: Vec::new(),
                }),
        );

        let contexts = kube::config::Kubeconfig::read()
            .map(|config| {
                config
                    .contexts
                    .into_iter()
                    .map(|context| context.name)
                    .collect()
            })
            .unwrap_or_default();
        let mut namespaces = vec![ALL_NAMESPACES.to_owned()];
        if let Ok(known) = self.known_namespaces.read() {
            namespaces.extend(known.iter().cloned());
        }

        entries.extend([
            Entry {
                names: vec!["ctx".to_owned(), "context".to_owned()],
                about: "switch kube context",
                action: Command::SwitchContext,
                args: contexts,
            },
            Entry {
                names: vec!["ns".to_owned(), "namespace".to_owned()],
                about: "switch namespace",
                action: Command::SwitchNamespace,
                args: namespaces,
            },
            Entry {
                names: vec!["help".to_owned(), "keys".to_owned()],
                about: "keys for this view",
                action: Command::ShowHelp,
                args: Vec::new(),
            },
            Entry {
                names: vec!["error".to_owned()],
                about: "the last connection error",
                action: Command::ShowError,
                args: Vec::new(),
            },
            Entry {
                names: vec!["quit".to_owned(), "q".to_owned()],
                about: "quit",
                action: Command::Quit,
                args: Vec::new(),
            },
        ]);
        self.command_bar = Some(CommandBar::new(entries));
    }

    /// Runs `command` with the argument typed after it on the command line, if any.
    fn run_command_with(&mut self, command: Command, arg: Option<String>) {
        match (command, arg) {
            (Command::SwitchContext, Some(context)) => {
                // A new cluster invalidates the namespace override.
                _ = self.scope.send(Scope {
                    context: Some(context),
                    namespace: None,
                });
            }
            (Command::SwitchNamespace, Some(namespace)) => {
                self.scope.send_modify(|scope| {
                    scope.namespace = (namespace != ALL_NAMESPACES).then_some(namespace);
                });
            }
            (command, _) => self.run_command(command),
        }
    }

    fn run_command(&mut self, command: Command) {
        match command {
            Command::GoTo(screen) => self.go_to(screen),
            Command::Resource(index) => {
                self.resources_state.open(index);
                self.go_to(ActiveScreen::Resources);
            }
            Command::SwitchContext => self.open_context_picker(),
            Command::SwitchNamespace => self.open_namespace_picker(),
            Command::ShowError => {
                self.error_details = matches!(&*self.client.borrow(), Some(Err(_)));
            }
            Command::ShowHelp => self.help = true,
            Command::Quit => self.request_quit(),
        }
    }

    fn screens(&self) -> [&dyn Screen; ActiveScreen::COUNT] {
        [
            &self.home_state,
            &self.run_state,
            &self.targets_state,
            &self.config_state,
            &self.sessions_state,
            &self.databases_state,
            &self.queues_state,
            &self.preview_envs_state,
            &self.resources_state,
            &self.terminal_state,
        ]
    }

    fn screens_mut(&mut self) -> [&mut dyn Screen; ActiveScreen::COUNT] {
        [
            &mut self.home_state,
            &mut self.run_state,
            &mut self.targets_state,
            &mut self.config_state,
            &mut self.sessions_state,
            &mut self.databases_state,
            &mut self.queues_state,
            &mut self.preview_envs_state,
            &mut self.resources_state,
            &mut self.terminal_state,
        ]
    }

    /// Quits, unless a screen holds edits that quitting would lose, in which case it asks first.
    fn request_quit(&mut self) {
        let unsaved: Vec<String> = self
            .screens()
            .iter()
            .filter_map(|screen| screen.unsaved_changes())
            .collect();
        if unsaved.is_empty() {
            self.quit = true;
        } else {
            self.quit_confirm = Some(QuitConfirm {
                unsaved,
                error: None,
            });
        }
    }

    /// `s` in the quit dialog: saves every screen's edits, and quits once all of them are saved.
    /// A save that fails keeps the dialog open with the reason, so nothing is lost silently.
    fn save_and_quit(&mut self) {
        let mut errors = Vec::new();
        for screen in self.screens_mut() {
            if screen.unsaved_changes().is_some()
                && let Err(error) = screen.save_changes()
            {
                errors.push(error);
            }
        }

        if errors.is_empty() {
            self.quit = true;
        } else if let Some(confirm) = &mut self.quit_confirm {
            confirm.error = Some(errors.join("; "));
        }
    }

    /// Draws the dialog asking whether to quit with unsaved edits.
    fn draw_quit_confirm(&self, frame: &mut Frame, area: Rect, confirm: &QuitConfirm) {
        const WIDTH: u16 = 64;
        let inner_width = usize::from(WIDTH.saturating_sub(4));

        let mut lines = Vec::new();
        for unsaved in &confirm.unsaved {
            for line in crate::helpers::wrap(unsaved, inner_width) {
                lines.push(Line::styled(format!(" {line}"), theme::warning()));
            }
        }
        if let Some(error) = &confirm.error {
            lines.push(Line::raw(""));
            for line in crate::helpers::wrap(&format!("Not saved: {error}"), inner_width) {
                lines.push(Line::styled(format!(" {line}"), theme::error()));
            }
        }
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            " s save and quit · q quit without saving · esc stay",
            theme::muted().italic(),
        ));

        let height = (lines.len() as u16 + 2).min(area.height);
        let inner = dialog(
            frame,
            centered(area, Constraint::Length(WIDTH), Constraint::Length(height)),
            "Unsaved changes",
        );
        frame.render_widget(Paragraph::new(lines), inner);
    }

    fn go_to(&mut self, screen: ActiveScreen) {
        self.active = screen;
        self.telemetry.tab_visited(screen.label());
    }

    /// The keys the active screen answers to right now.
    fn screen_hints(&self) -> Vec<Hint> {
        match self.active {
            ActiveScreen::Home => self.home_state.hints(),
            ActiveScreen::Run => self.run_state.hints(),
            ActiveScreen::Targets => self.targets_state.hints(),
            ActiveScreen::Config => self.config_state.hints(),
            ActiveScreen::Resources => self.resources_state.hints(),
            ActiveScreen::Sessions => self.sessions_state.hints(),
            ActiveScreen::Databases => self.databases_state.hints(),
            ActiveScreen::Queues => self.queues_state.hints(),
            ActiveScreen::PreviewEnvs => self.preview_envs_state.hints(),
            ActiveScreen::Terminal => self.terminal_state.hints(),
        }
    }

    /// What the bottom key bar shows: the keys of whatever has the keyboard, then the two ways
    /// to everything else.
    fn key_hints(&self) -> Vec<Hint> {
        if self.command_bar.is_some() {
            return vec![
                Hint::new("tab", "complete"),
                Hint::new("enter", "run"),
                Hint::new("esc", "cancel"),
            ];
        }
        // `?` comes first so that it is never the one cut off on a narrow terminal: it lists
        // everything else.
        let mut keys = vec![Hint::new("?", "help")];
        keys.extend(self.screen_hints());
        keys.push(Hint::new(":", "go to"));
        keys
    }

    /// Draws the `?` dialog: the active screen's keys, then the keys that work everywhere.
    fn draw_help(&self, frame: &mut Frame, area: Rect) {
        /// Width of the key column, so the actions line up.
        const KEY_WIDTH: usize = 18;

        let row = |hint: &Hint| {
            Line::from_iter([
                Span::styled(
                    format!("   {:<KEY_WIDTH$}", hint.keys),
                    Style::default().fg(theme::LAVENDER),
                ),
                Span::styled(hint.action.clone(), theme::muted()),
            ])
        };

        let mut lines = Vec::new();
        let screen = self.screen_hints();
        if !screen.is_empty() {
            lines.push(Line::styled(
                format!(" {}", self.active.label()),
                theme::title(),
            ));
            lines.extend(screen.iter().map(row));
            lines.push(Line::raw(""));
        }
        lines.push(Line::styled(" Everywhere", theme::title()));
        lines.extend(GLOBAL_HINTS.iter().map(row));
        lines.push(Line::raw(""));
        lines.push(Line::styled(" esc to close", theme::muted().italic()));

        // The lines plus the two borders.
        let height = (lines.len() as u16 + 2).min(area.height);
        let inner = dialog(
            frame,
            centered(area, Constraint::Length(60), Constraint::Length(height)),
            "Keys",
        );
        frame.render_widget(Paragraph::new(lines), inner);
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
        let known_namespaces = self.known_namespaces.clone();

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

            // For `:ns` to suggest. Best effort: without permission to list namespaces, a
            // namespace can still be typed in full.
            if let Ok(built) = &result
                && let Ok(list) =
                    kube::Api::<k8s_openapi::api::core::v1::Namespace>::all(built.clone())
                        .list(&Default::default())
                        .await
                && let Ok(mut known) = known_namespaces.write()
            {
                *known = list
                    .items
                    .into_iter()
                    .filter_map(|namespace| namespace.metadata.name)
                    .collect();
                known.sort();
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

    fn draw(&mut self, frame: &mut Frame) {
        let command_height = u16::from(self.command_bar.is_some());
        let [main_area, command_area, keys_area] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(command_height),
            Constraint::Length(1),
        ])
        .areas(frame.area());
        let [sidebar_area, body] = Layout::horizontal([
            Constraint::Length(Sidebar::width(main_area.width)),
            Constraint::Min(1),
        ])
        .areas(main_area);

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
            ActiveScreen::Run => self.run_state.draw(frame, body),
            ActiveScreen::Targets => self.targets_state.draw(frame, body),
            ActiveScreen::Config => self.config_state.draw(frame, body),
            ActiveScreen::Resources => self.resources_state.draw(frame, body),
            ActiveScreen::Sessions => self.sessions_state.draw(frame, body),
            ActiveScreen::Databases => self.databases_state.draw(frame, body),
            ActiveScreen::Queues => self.queues_state.draw(frame, body),
            ActiveScreen::PreviewEnvs => self.preview_envs_state.draw(frame, body),
            ActiveScreen::Terminal => self.terminal_state.draw(frame, body),
        }

        let scope = self.scope.borrow();
        let client = self.client.borrow();

        let counts = self.home_state.counts();
        let items: Vec<Item> = ActiveScreen::VARIANTS
            .iter()
            .map(|screen| Item {
                icon: screen.icon(),
                label: screen.short_label(),
                group: screen.group(),
                count: match screen {
                    ActiveScreen::Sessions => counts.sessions,
                    ActiveScreen::PreviewEnvs => counts.previews,
                    ActiveScreen::Queues => counts.splits,
                    ActiveScreen::Databases => counts.branches,
                    _ => None,
                },
                active: *screen as usize == self.active as usize,
            })
            .collect();
        let footer = status::sidebar_footer(&scope, &client, sidebar_area.width.saturating_sub(2));
        let switch = Switch {
            on: *self.local_only.borrow(),
            sessions: self
                .local_sessions
                .borrow()
                .as_ref()
                .map_or(0, LocalSessions::count),
        };
        self.sidebar
            .draw(frame, sidebar_area, &items, switch, footer);
        frame.render_widget(Paragraph::new(hints::key_bar(&self.key_hints())), keys_area);

        // What the status bar had to cut short, on request. Never over a picker, which owns the
        // keyboard while it is open and would leave this dialog undismissable.
        if let (true, Some(Err(error)), None) = (self.error_details, &*client, &self.picker) {
            status::draw_details(frame, body, error);
        }

        if self.help && self.picker.is_none() {
            self.draw_help(frame, body);
        }

        if let Some(confirm) = &self.quit_confirm {
            self.draw_quit_confirm(frame, body, confirm);
        }

        if let Some(bar) = &mut self.command_bar {
            bar.draw(frame, command_area);
        }

        if let Some(
            AppPicker::Context(picker) | AppPicker::Namespace(picker) | AppPicker::Command(picker),
        ) = &self.picker
        {
            picker.draw(frame, body);
        }

        theme::adapt(frame.buffer_mut());
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

        // A click on a screen in the sidebar goes there, unless a dialog is open over everything.
        if let Event::Mouse(mouse) = &event
            && self.picker.is_none()
            && self.command_bar.is_none()
            && self.quit_confirm.is_none()
            && !self.help
            && let Some(click) = self.sidebar.clicked(mouse)
        {
            match click {
                Click::Switch => self.toggle_mine(),
                Click::Screen(index) => {
                    if let Some(screen) = ActiveScreen::VARIANTS.get(index) {
                        self.go_to(*screen);
                    }
                }
            }
            return;
        }

        // An open picker owns the keyboard until it closes.
        if self.picker.is_some() {
            let Event::Key(key) = event else { return };
            let outcome = match &mut self.picker {
                Some(
                    AppPicker::Context(picker)
                    | AppPicker::Namespace(picker)
                    | AppPicker::Command(picker),
                ) => picker.handle_key(key),
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
                    Some(AppPicker::Command(_)) => {
                        if let Some(command) =
                            Command::all().find(|command| command.label() == choice)
                        {
                            self.run_command(command);
                        }
                    }
                    None => {}
                },
            }
            return;
        }

        // The command line owns the keyboard while it is open.
        if let Some(bar) = &mut self.command_bar {
            match bar.handle_event(&event) {
                CommandOutcome::Open => {}
                CommandOutcome::Closed => self.command_bar = None,
                CommandOutcome::Run(command, arg) => {
                    self.command_bar = None;
                    self.run_command_with(command, arg);
                }
            }
            return;
        }

        // The quit dialog sits over everything, and only ever ends in quitting or staying.
        if self.quit_confirm.is_some() {
            let Event::Key(key) = event else { return };
            match key.code {
                KeyCode::Char('s') => self.save_and_quit(),
                KeyCode::Char('q') => self.quit = true,
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.quit = true;
                }
                KeyCode::Esc => self.quit_confirm = None,
                _ => {}
            }
            return;
        }

        // So is the help dialog, which is only ever opened over the screen it describes.
        if self.help {
            let Event::Key(key) = event else { return };
            if matches!(
                key.code,
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?')
            ) {
                self.help = false;
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
            ActiveScreen::Run => self.run_state.handle_event(event),
            ActiveScreen::Targets => self.targets_state.handle_event(event),
            ActiveScreen::Config => self.config_state.handle_event(event),
            ActiveScreen::Resources => self.resources_state.handle_event(event),
            ActiveScreen::Sessions => self.sessions_state.handle_event(event),
            ActiveScreen::Databases => self.databases_state.handle_event(event),
            ActiveScreen::Queues => self.queues_state.handle_event(event),
            ActiveScreen::PreviewEnvs => self.preview_envs_state.handle_event(event),
            ActiveScreen::Terminal => self.terminal_state.handle_event(event),
        };

        // A screen can send the user to another view, such as the dashboard's `1` to sessions.
        let requested = match self.active {
            ActiveScreen::Home => self.home_state.take_view_request(),
            _ => None,
        };
        if let Some(name) = requested
            && let Some(screen) = ActiveScreen::VARIANTS
                .iter()
                .find(|screen| screen.names().first() == Some(&name))
        {
            self.go_to(*screen);
        }

        let ControlFlow::Continue(Event::Key(event)) = event else {
            return;
        };

        match event.code {
            KeyCode::Char('q') => self.request_quit(),
            KeyCode::Char('c') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.request_quit();
            }
            KeyCode::Char('p') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.open_command_palette();
            }
            KeyCode::Char(':') => self.open_command_bar(),
            KeyCode::Char('m') => self.toggle_mine(),
            KeyCode::Char('?') => self.help = true,
            // Only worth opening over a failure - with nothing to explain, `e` is free for the
            // screens to use.
            KeyCode::Char('e') => {
                self.error_details = matches!(&*self.client.borrow(), Some(Err(_)));
            }
            // Scope pickers work from every screen (when the screen didn't
            // claim the key for itself).
            KeyCode::Char('c') => self.open_context_picker(),
            KeyCode::Char('n') => self.open_namespace_picker(),
            // Some terminals report Shift+Tab as Tab with Shift held rather than as BackTab.
            KeyCode::BackTab => self.step_screen(false),
            KeyCode::Tab if event.modifiers.contains(KeyModifiers::SHIFT) => {
                self.step_screen(false)
            }
            KeyCode::Tab => self.step_screen(true),
            _ => {}
        }
    }
}

impl App {
    /// `m`, or a click on the sidebar's switch: every screen shows only what the sessions running
    /// on this machine made, or everything again.
    fn toggle_mine(&mut self) {
        self.local_only.send_modify(|on| *on = !*on);
    }

    /// Moves to the next or previous screen, wrapping around. The Terminal is left out: its
    /// shell takes every key, Tab included, so cycling into it would leave no way to cycle out.
    /// `:term` reaches it.
    fn step_screen(&mut self, forward: bool) {
        let count = ActiveScreen::COUNT;
        let mut index = self.active as usize;
        loop {
            index = if forward {
                (index + 1) % count
            } else {
                (index + count - 1) % count
            };
            match ActiveScreen::from_repr(index) {
                Some(ActiveScreen::Terminal) => continue,
                Some(screen) => return self.go_to(screen),
                None => return,
            }
        }
    }
}

#[derive(
    Clone, Copy, strum_macros::FromRepr, strum_macros::EnumCount, strum_macros::VariantArray,
)]
#[repr(usize)]
enum ActiveScreen {
    Home,
    Run,
    Targets,
    Sessions,
    PreviewEnvs,
    Queues,
    Databases,
    Config,
    Resources,
    Terminal,
}

impl ActiveScreen {
    /// The sign the sidebar draws before the screen's name. Each is one column wide on every
    /// terminal: no emoji, which some terminals draw two columns wide.
    fn icon(self) -> &'static str {
        match self {
            ActiveScreen::Home => "\u{2302}",
            ActiveScreen::Run => "\u{25b6}",
            ActiveScreen::Targets => "\u{25ce}",
            ActiveScreen::Sessions => "\u{25c9}",
            ActiveScreen::PreviewEnvs => "\u{25c7}",
            ActiveScreen::Queues => "\u{224b}",
            ActiveScreen::Databases => "\u{25a4}",
            ActiveScreen::Config => "\u{2261}",
            ActiveScreen::Resources => "\u{25a6}",
            ActiveScreen::Terminal => "\u{276f}",
        }
    }

    /// The screen's name in the sidebar, short enough for its width.
    fn short_label(self) -> &'static str {
        match self {
            ActiveScreen::PreviewEnvs => "Previews",
            other => other.label(),
        }
    }

    /// The sidebar heading the screen sits under: starting a session, watching what runs, or
    /// setting things up.
    fn group(self) -> Option<&'static str> {
        match self {
            ActiveScreen::Home => None,
            ActiveScreen::Run | ActiveScreen::Targets => Some("start"),
            ActiveScreen::Sessions
            | ActiveScreen::PreviewEnvs
            | ActiveScreen::Queues
            | ActiveScreen::Databases => Some("watch"),
            ActiveScreen::Config | ActiveScreen::Resources | ActiveScreen::Terminal => {
                Some("setup")
            }
        }
    }

    /// What `:` takes to reach this screen, the first being the name it suggests.
    fn names(self) -> &'static [&'static str] {
        match self {
            ActiveScreen::Home => &["home"],
            ActiveScreen::Run => &["run", "start", "go"],
            ActiveScreen::Targets => &["targets", "up", "plan"],
            ActiveScreen::Config => &["config", "cfg"],
            ActiveScreen::Sessions => &["sessions", "ss", "session"],
            ActiveScreen::Databases => &["dbs", "databases", "branches"],
            ActiveScreen::Queues => &["queues", "qs", "splits"],
            ActiveScreen::PreviewEnvs => &["previews", "pv", "preview"],
            ActiveScreen::Resources => &["resources", "crds", "api"],
            ActiveScreen::Terminal => &["terminal", "term", "shell"],
        }
    }

    /// What the screen shows, next to its name in the `:` suggestions.
    fn about(self) -> &'static str {
        match self {
            ActiveScreen::Home => "welcome screen",
            ActiveScreen::Run => "start a session from this folder",
            ActiveScreen::Targets => "pick several targets and run them together",
            ActiveScreen::Config => "edit .mirrord/mirrord.json",
            ActiveScreen::Sessions => "mirrord sessions on the cluster",
            ActiveScreen::Databases => "branch databases",
            ActiveScreen::Queues => "queue splits",
            ActiveScreen::PreviewEnvs => "preview environments",
            ActiveScreen::Resources => "every mirrord resource type on the cluster",
            ActiveScreen::Terminal => "a shell with its mirrord sessions",
        }
    }

    /// The tab's name in the header, and the key its visits are counted under in telemetry.
    fn label(self) -> &'static str {
        match self {
            ActiveScreen::Home => "Home",
            ActiveScreen::Run => "Run",
            ActiveScreen::Targets => "Targets",
            ActiveScreen::Config => "Config",
            ActiveScreen::Sessions => "Sessions",
            ActiveScreen::Databases => "Databases",
            ActiveScreen::Queues => "Queues",
            ActiveScreen::PreviewEnvs => "Preview Environments",
            ActiveScreen::Resources => "Resources",
            ActiveScreen::Terminal => "Terminal",
        }
    }
}
