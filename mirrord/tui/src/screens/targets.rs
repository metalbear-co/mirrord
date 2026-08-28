//! The targets screen: a wizard that browses everything mirrord can target,
//! accumulates picks into an ordered `mirrord-up.yaml` plan, exports it, and
//! runs `mirrord up` on it.

use std::{ops::ControlFlow, path::PathBuf};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};

use crate::{
    context::Context,
    helpers::{centered, dialog},
    screens::{
        Screen,
        targets::{
            browser::{Browser, BrowserOutcome},
            form::{
                FormOutcome, SERVICE_SETTINGS, SettingsForm, draft_for_target, join_command,
                validate_service,
            },
            launch::{Conflict, Launch},
            model::{ServiceEntry, TargetSpec},
            plan::{EXPORT_SETTINGS, ExportDraft, PlanPane, validate_export, write_export},
        },
    },
};

mod browser;
mod form;
mod history;
mod keys;
mod launch;
mod model;
mod plan;
mod suggest;
mod theme;

/// Which pane has the keyboard: the browser (left), the session plan
/// (right), or the `mirrord up` log pane that opens below both while a
/// run pane exists.
#[derive(Clone, Copy, Eq, PartialEq)]
enum Focus {
    Browser,
    Plan,
    Logs,
}

/// A dialog floating over the panes.
enum Modal {
    Service {
        form: SettingsForm<ServiceEntry>,
        /// Index in the plan when editing an existing service.
        editing: Option<usize>,
    },
    Export {
        form: SettingsForm<ExportDraft>,
    },
}

impl Modal {
    fn handle_key(&mut self, key: KeyEvent) -> FormOutcome {
        match self {
            Self::Service { form, .. } => form.handle_key(key),
            Self::Export { form } => form.handle_key(key),
        }
    }

    fn typing(&self) -> bool {
        match self {
            Self::Service { form, .. } => form.typing(),
            Self::Export { form } => form.typing(),
        }
    }

    fn paste(&mut self, text: &str) -> bool {
        match self {
            Self::Service { form, .. } => form.paste(text),
            Self::Export { form } => form.paste(text),
        }
    }
}

/// Transient one-line message shown in place of the hint bar until the next
/// key press.
struct Status {
    text: String,
    warning: bool,
}

/// The targets screen.
pub struct TargetsScreen {
    context: Context,
    browser: Browser,
    plan: PlanPane,
    focus: Focus,
    modal: Option<Modal>,
    launch: Option<Launch>,
    /// Remembered across exports and runs: path, format, and the `common`
    /// section of the emitted file.
    export: ExportDraft,
    status: Option<Status>,
    /// A failed run blamed on lingering sessions; the kill-and-relaunch
    /// dialog is open while this is set.
    conflict: Option<Conflict>,
    /// The user dismissed the pre-run session prompt: the next run skips
    /// the check instead of asking again.
    skip_session_check: bool,
    /// The context the history module last saw, to notice scope switches.
    seen_context: Option<String>,
    /// One-shot note for the next run's log pane (e.g. what cleanup
    /// preceded it).
    launch_note: Option<String>,
    /// The `?` key binding overlay is open.
    help: bool,
}

impl TargetsScreen {
    fn open_service_form(&mut self, draft: ServiceEntry, editing: Option<usize>) {
        let (title, finish) = if editing.is_some() {
            (" Edit Service ", "Save")
        } else {
            (" Add Service ", "Add to plan")
        };
        self.modal = Some(Modal::Service {
            form: SettingsForm::new(draft, SERVICE_SETTINGS, title, finish, validate_service),
            editing,
        });
    }

    fn open_export_form(&mut self) {
        if self.plan.services.is_empty() {
            self.warn("nothing to export - pick a target on the left first");
            return;
        }
        self.modal = Some(Modal::Export {
            form: SettingsForm::new(
                self.export.clone(),
                EXPORT_SETTINGS,
                " Export ",
                "Write file",
                validate_export,
            ),
        });
    }

    fn run(&mut self) {
        if self.launch.as_ref().is_some_and(Launch::running) {
            self.warn("already running - stop it first (x)");
            return;
        }
        if self.plan.services.iter().all(|service| service.spec.skip) {
            self.warn("nothing to run - the plan is empty or all services skip");
            return;
        }
        // The command becomes required only now: it is what actually gets
        // launched locally. A broken directory or command drops the user
        // straight into that service's offending field with the reason
        // shown; mirrord up itself would only fail after the whole session
        // is set up.
        let service_dir =
            |service: &ServiceEntry| PathBuf::from(service.spec.run.dir.as_deref().unwrap_or("."));
        let broken = self.plan.services.iter().position(|service| {
            if service.spec.skip {
                return false;
            }
            let dir = service_dir(service);
            match service.spec.run.command.first() {
                None => true,
                Some(binary) => !dir.is_dir() || !launch::command_found(binary, &dir),
            }
        });
        if let Some(index) = broken {
            let service = self.plan.services[index].clone();
            let dir = service_dir(&service);
            let (field, error) = if !dir.is_dir() {
                (
                    "Directory",
                    format!("directory `{}` does not exist", dir.display()),
                )
            } else {
                match service.spec.run.command.first() {
                    None => (
                        "Command",
                        format!(
                            "`{}` needs a command to run - how do you start it locally? \
                             Tab completes paths and detected commands",
                            service.name
                        ),
                    ),
                    Some(binary) => (
                        "Command",
                        format!("`{binary}` not found - use a full path or a binary on PATH"),
                    ),
                }
            };
            self.open_service_form(service, Some(index));
            if let Some(Modal::Service { form, .. }) = &mut self.modal {
                form.edit_field(field);
                form.set_error(error);
            }
            return;
        }

        // Lingering sessions on this plan's targets hold the traffic lock
        // and would get the new run SIGTERMed - ask about them first.
        // (Blocks for one operator API call; Esc on the prompt skips the
        // check for the next run.)
        if self.skip_session_check {
            self.skip_session_check = false;
        } else {
            let targets: Vec<(String, Option<String>)> = self
                .plan
                .services
                .iter()
                .filter(|service| !service.spec.skip)
                .filter_map(|service| match &service.spec.target {
                    TargetSpec::Path { path, namespace } => Some((path.clone(), namespace.clone())),
                    TargetSpec::Targetless => None,
                })
                .collect();
            let sessions = launch::lingering_sessions(&targets);
            if !sessions.is_empty() {
                self.conflict = Some(Conflict {
                    sessions,
                    pre_launch: true,
                });
                return;
            }
        }

        let file = self.plan.up_file(&self.export.common);
        match Launch::start(self.context.clone(), &file, self.launch_note.take()) {
            Ok(launch) => {
                // What launches becomes each target's history, feeding the
                // prefill and suggestions next time it is picked.
                for service in self.plan.services.iter().filter(|s| !s.spec.skip) {
                    history::record(
                        history::target_key(&service.spec.target),
                        service.spec.run.dir.clone(),
                        join_command(&service.spec.run.command),
                    );
                }
                self.launch = Some(launch);
                self.focus = Focus::Logs;
            }
            Err(error) => self.warn(&error.to_string()),
        }
    }

    fn warn(&mut self, text: &str) {
        self.status = Some(Status {
            text: text.to_owned(),
            warning: true,
        });
    }

    fn info(&mut self, text: String) {
        self.status = Some(Status {
            text,
            warning: false,
        });
    }

    fn finish_modal(&mut self, modal: Modal) {
        match modal {
            Modal::Service { form, editing } => {
                self.plan.upsert(form.draft, editing);
                self.focus = Focus::Plan;
            }
            Modal::Export { mut form } => {
                let file = self.plan.up_file(&form.draft.common);
                match write_export(&form.draft, &file) {
                    Ok(path) => {
                        self.export = form.draft;
                        self.info(format!("wrote {}", path.display()));
                    }
                    Err(error) => {
                        form.set_error(error.to_string());
                        self.modal = Some(Modal::Export { form });
                    }
                }
            }
        }
    }

    fn handle_browser_key(&mut self, key: KeyEvent) -> ControlFlow<(), Event> {
        match self.browser.handle_key(key) {
            BrowserOutcome::Pick(picked) => {
                let draft =
                    draft_for_target(&picked.path, &picked.namespace, &picked.workload_name);
                self.open_service_form(draft, None);
                ControlFlow::Break(())
            }
            BrowserOutcome::Consumed => ControlFlow::Break(()),
            BrowserOutcome::Ignored => match key.code {
                KeyCode::Char(keys::FOCUS_PLAN) => {
                    self.focus = Focus::Plan;
                    ControlFlow::Break(())
                }
                _ => ControlFlow::Continue(Event::Key(key)),
            },
        }
    }

    fn handle_plan_key(&mut self, key: KeyEvent) -> ControlFlow<(), Event> {
        match key.code {
            // Stopping works from the plan too, so relaunching is
            // stop-then-run without leaving the pane.
            KeyCode::Char(keys::STOP) => {
                if let Some(launch) = &mut self.launch {
                    launch.stop();
                }
            }
            KeyCode::Up | KeyCode::Char(keys::UP) => self.plan.select_up(),
            KeyCode::Down | KeyCode::Char(keys::DOWN) => self.plan.select_down(),
            KeyCode::Char(keys::MOVE_UP) => self.plan.move_up(),
            KeyCode::Char(keys::MOVE_DOWN) => self.plan.move_down(),
            KeyCode::Char(keys::DELETE) => self.plan.delete_selected(),
            KeyCode::Char(keys::SKIP) => self.plan.toggle_skip_selected(),
            KeyCode::Enter => {
                if let Some(service) = self.plan.services.get(self.plan.selected) {
                    self.open_service_form(service.clone(), Some(self.plan.selected));
                }
            }
            KeyCode::Char(keys::EXPORT) => self.open_export_form(),
            KeyCode::Char(keys::RUN) => self.run(),
            KeyCode::Char(keys::BROWSE) | KeyCode::Esc => self.focus = Focus::Browser,
            _ => return ControlFlow::Continue(Event::Key(key)),
        }
        ControlFlow::Break(())
    }

    fn handle_logs_key(&mut self, key: KeyEvent) -> ControlFlow<(), Event> {
        let Some(launch) = &mut self.launch else {
            self.focus = Focus::Plan;
            return ControlFlow::Continue(Event::Key(key));
        };

        match key.code {
            KeyCode::Char(keys::STOP | keys::UP | keys::DOWN | keys::FOLLOW)
            | KeyCode::Up
            | KeyCode::Down
            | KeyCode::End => {
                if launch.handle_key(key) {
                    self.launch = None;
                    self.focus = Focus::Plan;
                }
            }
            // While running, Esc steps back to the plan; once exited it
            // closes the pane (handled by the launch itself).
            KeyCode::Esc => {
                if launch.running() {
                    self.focus = Focus::Plan;
                } else if launch.handle_key(key) {
                    self.launch = None;
                    self.focus = Focus::Plan;
                }
            }
            KeyCode::Char(keys::BROWSE) => self.focus = Focus::Browser,
            KeyCode::Char(keys::FOCUS_PLAN) => self.focus = Focus::Plan,
            KeyCode::Char(keys::RUN) => self.run(),
            _ => return ControlFlow::Continue(Event::Key(key)),
        }
        ControlFlow::Break(())
    }

    fn hint(&self) -> String {
        if self.conflict.is_some() {
            return "Enter kill sessions & relaunch · Esc dismiss".to_owned();
        }
        if let Some(modal) = &self.modal {
            return if modal.typing() {
                "type the value · Enter apply · Esc cancel".to_owned()
            } else {
                "↑/↓ field · Enter edit/cycle · Esc back".to_owned()
            };
        }

        match self.focus {
            Focus::Browser => format!(
                "{} · / filter · n namespace · c context · p plan · ? help",
                self.browser.enter_hint()
            ),
            Focus::Plan => {
                if self.plan.services.is_empty() {
                    return "a browse · ? help".to_owned();
                }
                // Every per-service action, adapted to the selection.
                let skip = match self.plan.services.get(self.plan.selected) {
                    Some(service) if service.spec.skip => "s unskip",
                    _ => "s skip",
                };
                match &self.launch {
                    Some(launch) if launch.running() => format!(
                        "Enter edit · d delete · {skip} · K/J reorder · x stop · ⌃↓ logs · ? help"
                    ),
                    _ => format!(
                        "Enter edit · d delete · {skip} · K/J reorder · e export · r run · \
                         a browse · ? help"
                    ),
                }
            }
            Focus::Logs => match &self.launch {
                Some(launch) if launch.running() => {
                    "x stop · ↑/↓ scroll · p plan · ? help".to_owned()
                }
                _ => "Esc close · r rerun · ↑/↓ scroll · p plan · ? help".to_owned(),
            },
        }
    }

    /// The kill-and-relaunch dialog for a run sabotaged by lingering
    /// sessions.
    fn draw_conflict(&self, frame: &mut Frame, area: Rect, conflict: &Conflict) {
        let (headline, explain) = if conflict.pre_launch {
            (
                "Sessions already target this plan.",
                "They hold the traffic lock - the new run would be killed.",
            )
        } else {
            (
                "The run was killed by SIGTERM.",
                "A previous mirrord session likely still holds the traffic lock.",
            )
        };
        let mut lines = vec![
            Line::styled(headline, Style::default().fg(theme::TEXT_EMPHASIS)),
            Line::raw(""),
            Line::styled(explain, Style::default().fg(theme::TEXT_MUTED)),
            Line::raw(""),
        ];
        if conflict.sessions.is_empty() {
            lines.push(Line::styled(
                "Kill ALL your operator sessions and relaunch?",
                Style::default().fg(theme::TEXT_EMPHASIS).bold(),
            ));
        } else {
            lines.push(Line::styled(
                "Kill these sessions and launch?",
                Style::default().fg(theme::TEXT_EMPHASIS).bold(),
            ));
            for id in &conflict.sessions {
                lines.push(Line::styled(
                    format!("  {id}"),
                    Style::default().fg(theme::WARNING),
                ));
            }
        }
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            if conflict.pre_launch {
                "Enter kill & launch · Esc launch anyway next r"
            } else {
                "Enter kill & relaunch · Esc dismiss"
            },
            Style::default().fg(theme::TEXT_MUTED).italic(),
        ));

        let dialog_area = centered(
            area,
            Constraint::Length(58),
            Constraint::Length(lines.len() as u16 + 2),
        );
        let inner = dialog(frame, dialog_area, " Lingering sessions ");
        frame.render_widget(Paragraph::new(lines), inner);
    }

    /// The `?` overlay: every binding of the screen, grouped by pane.
    fn draw_help(&self, frame: &mut Frame, area: Rect) {
        let header =
            |name: &str| Line::styled(format!(" {name}"), Style::default().fg(theme::BRAND).bold());
        let bind = |key: String, action: &str| {
            Line::from_iter([
                Span::styled(
                    format!("  {key:<8}"),
                    Style::default().fg(theme::TEXT_EMPHASIS),
                ),
                Span::styled(action.to_owned(), Style::default().fg(theme::TEXT_MUTED)),
            ])
        };

        let lines = vec![
            header("Browse"),
            bind("Enter".into(), "expand / pick the target under the cursor"),
            bind("→ / ←".into(), "expand / collapse containers"),
            bind("↑ / ↓".into(), "move the cursor (also j/k)"),
            bind(keys::FILTER.into(), "filter targets"),
            bind(keys::NAMESPACE.into(), "switch namespace"),
            bind(keys::CONTEXT.into(), "switch kube context (cluster)"),
            bind(keys::REFRESH.into(), "refresh from the cluster"),
            bind(keys::FOCUS_PLAN.into(), "jump to the plan"),
            Line::raw(""),
            header("Plan"),
            bind("Enter".into(), "edit the selected service"),
            bind(
                format!("{} / {}", keys::MOVE_UP, keys::MOVE_DOWN),
                "reorder services",
            ),
            bind(keys::DELETE.into(), "delete service"),
            bind(keys::SKIP.into(), "toggle skip"),
            bind(keys::EXPORT.into(), "export yaml/json"),
            bind(keys::RUN.into(), "run the plan"),
            bind(keys::BROWSE.into(), "add another target"),
            Line::raw(""),
            header("Run"),
            bind(keys::STOP.into(), "stop (again to force-kill)"),
            bind(keys::RUN.into(), "run again"),
            bind(keys::FOLLOW.into(), "follow the log tail"),
            Line::raw(""),
            bind("Esc".into(), "one step back, everywhere"),
            bind("⌃ ←/→".into(), "switch pane (also Alt)"),
            bind("⌃ ↑/↓".into(), "plan ↔ logs pane"),
            bind("⌃a/e/u/k/w".into(), "readline editing in text fields"),
        ];

        let dialog_area = centered(
            area,
            Constraint::Length(52),
            Constraint::Length(lines.len() as u16 + 2),
        );
        let inner = dialog(frame, dialog_area, " Keys ");
        frame.render_widget(Paragraph::new(lines), inner);
    }
}

impl Screen for TargetsScreen {
    fn new(context: Context) -> Self {
        Self {
            browser: Browser::new(context.clone()),
            plan: PlanPane::default(),
            context,
            focus: Focus::Browser,
            modal: None,
            launch: None,
            export: ExportDraft::default(),
            status: None,
            conflict: None,
            skip_session_check: false,
            seen_context: None,
            launch_note: None,
            help: false,
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        // Status messages (errors especially) and the fuller key hints can
        // be long; give them up to three wrapped lines instead of clipping
        // at the pane edge.
        let hint_text_length = match &self.status {
            Some(status) => status.text.chars().count(),
            None => self.hint().chars().count(),
        };
        let width = (area.width as usize).max(1);
        let hint_height = ((hint_text_length + 2).div_ceil(width) as u16).clamp(1, 3);
        let [main, hint_area] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(hint_height)]).areas(area);

        // The log pane opens below the browser and plan, so both stay
        // visible (and the plan stays editable) while a run is active.
        let (top, logs_area) = match self.launch {
            Some(_) => {
                let [top, bottom] =
                    Layout::vertical([Constraint::Percentage(60), Constraint::Percentage(40)])
                        .areas(main);
                (top, Some(bottom))
            }
            None => (main, None),
        };
        let [left, right] =
            Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(top);

        let modal_open = self.modal.is_some();
        self.browser
            .draw(frame, left, self.focus == Focus::Browser && !modal_open);

        match &mut self.modal {
            // The service form can grow many settings, so it takes over the
            // whole right pane instead of floating in a small dialog.
            Some(Modal::Service { form, .. }) => form.draw(frame, right),
            other => {
                self.plan
                    .draw(frame, right, self.focus == Focus::Plan && !modal_open);
                if let Some(Modal::Export { form }) = other {
                    let dialog_area = centered(
                        main,
                        Constraint::Length(64),
                        Constraint::Length(form.dialog_height(62)),
                    );
                    form.draw(frame, dialog_area);
                }
            }
        }

        if let (Some(launch), Some(logs_area)) = (&mut self.launch, logs_area) {
            launch.draw(frame, logs_area, self.focus == Focus::Logs && !modal_open);
        }

        // A run that died to a lingering-session conflict raises the
        // kill-and-relaunch dialog (once per run).
        if !modal_open
            && self.conflict.is_none()
            && let Some(launch) = &mut self.launch
            && let Some(conflict) = launch.conflict()
        {
            self.conflict = Some(conflict);
        }
        if let Some(conflict) = &self.conflict {
            self.draw_conflict(frame, main, conflict);
        }
        // History keys follow whatever cluster the app-level picker (or
        // anything else) switches the scope to.
        let scope_context = self.context.scope.borrow().context.clone();
        if scope_context != self.seen_context {
            self.seen_context = scope_context.clone();
            history::set_cluster(scope_context);
        }

        if self.help {
            self.draw_help(frame, main);
        }

        let hint = match &self.status {
            Some(status) => Line::styled(
                format!(" {}", status.text),
                if status.warning {
                    Style::default().fg(theme::WARNING)
                } else {
                    Style::default().fg(theme::BRAND)
                },
            ),
            // Keys light up, their labels stay muted: `x stop · G follow`.
            None => {
                let hint = self.hint();
                let mut spans = vec![Span::raw(" ")];
                for (index, segment) in hint.split(" · ").enumerate() {
                    if index > 0 {
                        spans.push(Span::styled(" · ", Style::default().fg(theme::BORDER_DIM)));
                    }
                    let (key, label) = segment.split_once(' ').unwrap_or((segment, ""));
                    spans.push(Span::styled(
                        key.to_owned(),
                        Style::default().fg(theme::TEXT_EMPHASIS),
                    ));
                    if !label.is_empty() {
                        spans.push(Span::styled(
                            format!(" {label}"),
                            Style::default().fg(theme::TEXT_MUTED).italic(),
                        ));
                    }
                }
                Line::from_iter(spans)
            }
        };
        frame.render_widget(Paragraph::new(hint).wrap(Wrap { trim: false }), hint_area);
    }

    fn handle_event(&mut self, event: Event) -> ControlFlow<(), Event> {
        // Bracketed paste arrives as its own event, not keystrokes; route
        // it into whichever text editor is active.
        if let Event::Paste(text) = &event {
            let pasted = match &mut self.modal {
                Some(modal) => modal.paste(text),
                None => self.browser.paste(text),
            };
            if pasted {
                return ControlFlow::Break(());
            }
        }

        let Event::Key(key) = event else {
            return ControlFlow::Continue(event);
        };
        if key.kind == KeyEventKind::Release {
            return ControlFlow::Continue(Event::Key(key));
        }
        // Ctrl-C always reaches the app so quitting can't get trapped.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return ControlFlow::Continue(Event::Key(key));
        }

        self.status = None;

        // The help overlay swallows the next key press.
        if self.help {
            self.help = false;
            return ControlFlow::Break(());
        }

        if let Some(conflict) = &self.conflict {
            match key.code {
                KeyCode::Enter => {
                    let sessions = conflict.sessions.clone();
                    self.conflict = None;
                    // Blocks for the operator API calls; a beat of frozen
                    // UI beats juggling another async state machine here.
                    match launch::kill_sessions(&sessions) {
                        Ok(()) => {
                            // Told in the new run's log pane, not the
                            // status bar - a status message would sit
                            // stale until the next key press.
                            self.launch_note = Some(match sessions.len() {
                                0 => "note: killed all lingering sessions first".to_owned(),
                                count => {
                                    format!("note: killed {count} lingering session(s) first")
                                }
                            });
                            self.launch = None;
                            self.run();
                        }
                        Err(error) => self.warn(&format!("failed to kill sessions: {error}")),
                    }
                }
                KeyCode::Esc => {
                    // Declining the pre-run prompt means "launch anyway":
                    // the next `r` skips the check instead of nagging.
                    self.skip_session_check = conflict.pre_launch;
                    self.conflict = None;
                }
                _ => {}
            }
            return ControlFlow::Break(());
        }

        if let Some(mut modal) = self.modal.take() {
            match modal.handle_key(key) {
                FormOutcome::Consumed => self.modal = Some(modal),
                FormOutcome::Cancelled => {}
                FormOutcome::Finished => self.finish_modal(modal),
            }
            return ControlFlow::Break(());
        }

        // `?` is a plain character while the filter is being typed.
        if key.code == KeyCode::Char(keys::HELP) && !self.browser.typing() {
            self.help = true;
            return ControlFlow::Break(());
        }

        // Modified ←/→ jumps straight between the panes from anywhere.
        // Ctrl and Alt work in every terminal; Cmd only reaches the app in
        // terminals speaking the kitty keyboard protocol, most eat it.
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
        {
            match key.code {
                KeyCode::Left => {
                    self.focus = match self.focus {
                        Focus::Logs => Focus::Plan,
                        _ => Focus::Browser,
                    };
                    return ControlFlow::Break(());
                }
                KeyCode::Right => {
                    self.focus = match self.focus {
                        Focus::Plan if self.launch.is_some() => Focus::Logs,
                        _ => Focus::Plan,
                    };
                    return ControlFlow::Break(());
                }
                KeyCode::Down if self.launch.is_some() => {
                    self.focus = Focus::Logs;
                    return ControlFlow::Break(());
                }
                KeyCode::Up if self.focus == Focus::Logs => {
                    self.focus = Focus::Plan;
                    return ControlFlow::Break(());
                }
                _ => {}
            }
        }

        match self.focus {
            Focus::Browser => self.handle_browser_key(key),
            Focus::Plan => self.handle_plan_key(key),
            Focus::Logs => self.handle_logs_key(key),
        }
    }
}
