//! The Run view: start a mirrord session from this folder with as few decisions as possible.
//!
//! Opening it is enough to get a runnable session: the job defaults to the one most people want
//! (only my requests reach my machine), the target is picked by matching the project's names
//! against what runs in the cluster, and the command is the one used last time or the one the
//! project files point to. Each guess is shown on its own card with the reason for it, `Enter`
//! runs it, and a single key changes any card. The session runs through `mirrord up`, so what the
//! view does is exactly what the CLI would do with the plan it shows.

use std::{
    collections::BTreeSet,
    ops::ControlFlow,
    path::PathBuf,
    sync::{Arc, RwLock},
};

use crossterm::event::{Event, KeyCode};
use futures_util::future::join_all;
use k8s_openapi::api::core::v1::Namespace;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Padding, Paragraph, Wrap},
};
use serde_json::{Value, json};
use strum::VariantArray;
use tokio::sync::watch;

use crate::{
    context::Context,
    helpers::ellipsize,
    kit::{hints::Hint, panel, state_view::Fetch, text_input::TextInput},
    screens::{
        Screen,
        targets::{
            browser,
            form::split_command,
            history,
            launch::Launch,
            model::{
                CommonSpec, RunSpec, RunType, ServiceEntry, ServiceMode, ServiceSpec, TargetSpec,
                UpFile,
            },
            suggest,
        },
    },
    theme,
    widgets::picker::{Picker, PickerOutcome},
};

mod cluster;
mod databases;
pub(crate) mod detect;

use cluster::TargetResources;
use detect::{Candidate, ProjectName};

/// What the user wants the session to do. The first one is the default.
#[derive(Clone, Copy, Debug, Eq, PartialEq, strum_macros::VariantArray)]
enum Job {
    /// Requests that carry this session's key reach the local process; everyone else's keep
    /// going to the cluster.
    OnlyMine,
    /// The local process gets all of the target's traffic: mirrord copies the target and scales
    /// the original down.
    TakeOver,
    /// No target: the local process reaches the cluster's network, files and env, and no traffic
    /// is redirected to it.
    NoTraffic,
}

impl Job {
    fn label(self) -> &'static str {
        match self {
            Self::OnlyMine => "Only my requests",
            Self::TakeOver => "Take over all traffic",
            Self::NoTraffic => "No incoming traffic",
        }
    }

    fn about(self) -> &'static str {
        match self {
            Self::OnlyMine => "Requests with your session key reach you. Safe on shared clusters.",
            Self::TakeOver => "Every request reaches you; the original is scaled down.",
            Self::NoTraffic => "Cluster network, files and env; no requests sent to you.",
        }
    }

    /// What happens to the target's traffic, for the plan summary.
    fn traffic(self) -> &'static str {
        match self {
            Self::OnlyMine => "only requests with your session key",
            Self::TakeOver => "all of it, original scaled down",
            Self::NoTraffic => "none",
        }
    }

    fn needs_target(self) -> bool {
        self != Self::NoTraffic
    }
}

/// What the background discovery found in the cluster.
#[derive(Default)]
struct Discovery {
    namespace: String,
    candidates: Fetch<Vec<Candidate>>,
}

/// What discovery found, copied out of the lock for one draw.
enum Found {
    Loading,
    Disconnected,
    Failed(String),
    Targets(usize),
}

/// A pick-one dialog open over the view, and what it picks.
enum Pick {
    Job,
    Target,
    Command,
}

/// The entry of the command picker that opens the editor instead of picking a command.
const TYPE_OWN: &str = "type my own command...";

pub struct RunScreen {
    context: Context,
    /// The folder the TUI was started in: where the project is, and where its command runs.
    dir: PathBuf,
    names: Vec<ProjectName>,
    /// Commands the project files point to, best guess first.
    detected_commands: Vec<String>,
    discovery: Arc<RwLock<Discovery>>,
    job: Job,
    /// The target the user picked, by [`Candidate::label`]; `None` follows the best match.
    target_choice: Option<String>,
    /// The command the user picked or typed; `None` follows history, then detection.
    command_choice: Option<String>,
    editor: Option<TextInput>,
    picker: Option<(Pick, Picker)>,
    launch: Option<Launch>,
    /// `y`: the plan card shows the YAML mirrord runs instead of its summary.
    show_yaml: bool,
    /// What inspecting the current target found: its databases, the team's presets, its queues.
    inspection: Arc<RwLock<Inspection>>,
    /// The target to inspect, handed to the background inspection whenever it changes.
    inspect: watch::Sender<Option<Candidate>>,
    /// Databases to branch, by [`DbChoice::key`]. Cleared when the target changes, since the
    /// keys belong to the target they were found on.
    branch: BTreeSet<String>,
    /// The `b` checklist of databases, while open.
    checklist: Option<Checklist>,
    /// Why the last run could not start.
    problem: Option<String>,
}

impl RunScreen {
    /// Lists the targets in the scope's namespace and ranks them, again whenever the connection
    /// or the scope changes.
    async fn discover(
        mut context: Context,
        dir: PathBuf,
        names: Vec<ProjectName>,
        discovery: Arc<RwLock<Discovery>>,
    ) {
        loop {
            let client = context
                .client
                .borrow_and_update()
                .as_ref()
                .and_then(|result| result.as_ref().ok())
                .cloned();

            let state = match client {
                None => (String::new(), Fetch::Disconnected),
                Some(client) => {
                    let scoped = context.scope.borrow_and_update().namespace.clone();
                    let (label, namespaces) = match scoped {
                        Some(namespace) => (namespace.clone(), vec![namespace]),
                        None => (
                            "all namespaces".to_owned(),
                            searched_namespaces(&client).await,
                        ),
                    };
                    let fetched = join_all(
                        namespaces
                            .iter()
                            .map(|namespace| browser::fetch(&client, namespace)),
                    )
                    .await;

                    let mut failures = Vec::new();
                    let mut listings = Vec::new();
                    for data in fetched {
                        for listing in data.kinds {
                            match listing.outcome {
                                Ok(items) => {
                                    listings.push((data.namespace.clone(), listing.kind, items))
                                }
                                Err(error) => failures.push(error),
                            }
                        }
                    }

                    let state = if listings.is_empty() && !failures.is_empty() {
                        Fetch::Failed(failures.join("; "))
                    } else {
                        let here = dir.display().to_string();
                        let ran_before = |namespace: &str, path: &str| {
                            let target = TargetSpec::Path {
                                path: path.to_owned(),
                                namespace: Some(namespace.to_owned()),
                            };
                            history::recall(&history::target_key(&target))
                                .is_some_and(|past| past.dir.as_deref() == Some(here.as_str()))
                        };
                        Fetch::Ready(detect::rank(&listings, &names, ran_before))
                    };
                    (label, state)
                }
            };

            if let Ok(mut slot) = discovery.write() {
                slot.namespace = state.0;
                slot.candidates = state.1;
            }
            context.request_redraw();

            tokio::select! {
                changed = context.client.changed() => if changed.is_err() { break },
                changed = context.scope.changed() => if changed.is_err() { break },
            }
        }
    }

    /// The target the session would run against: the user's pick, else the best match.
    fn target(&self) -> Option<Candidate> {
        let discovery = self.discovery.read().ok()?;
        let Fetch::Ready(candidates) = &discovery.candidates else {
            return None;
        };
        match &self.target_choice {
            Some(label) => candidates
                .iter()
                .find(|candidate| &candidate.label() == label)
                .cloned(),
            None => candidates.first().cloned(),
        }
    }

    fn target_spec(&self) -> TargetSpec {
        match (self.job.needs_target(), self.target()) {
            (true, Some(candidate)) => TargetSpec::Path {
                path: candidate.path(),
                namespace: Some(candidate.namespace.clone()),
            },
            _ => TargetSpec::Targetless,
        }
    }

    /// The command to run, and where it came from.
    fn command(&self) -> Option<(String, String)> {
        if let Some(command) = &self.command_choice {
            return Some((command.clone(), "your pick".to_owned()));
        }
        let past = history::recall(&history::target_key(&self.target_spec()));
        if let Some(command) = past.and_then(|past| past.commands.into_iter().next()) {
            return Some((command, "ran last time".to_owned()));
        }
        self.detected_commands
            .first()
            .map(|command| (command.clone(), "detected - check it".to_owned()))
    }

    /// The plan `mirrord up` runs, when the view has everything it needs.
    fn plan(&self) -> Result<UpFile, &'static str> {
        if self.job.needs_target() && self.target().is_none() {
            return Err("a target");
        }
        let (command, _) = self.command().ok_or("a command")?;
        let command = split_command(&command).map_err(|_| "a command that parses")?;
        let target = self.target_spec();
        let name = self
            .target()
            .filter(|_| self.job.needs_target())
            .map(|candidate| candidate.name)
            .unwrap_or_else(|| "local".to_owned());

        Ok(UpFile {
            common: CommonSpec {
                context: self.context.scope.borrow().context.clone(),
                ..CommonSpec::default()
            },
            services: vec![ServiceEntry {
                name,
                spec: ServiceSpec {
                    target,
                    default_mode: match self.job {
                        Job::TakeOver => ServiceMode::Replace,
                        Job::OnlyMine | Job::NoTraffic => ServiceMode::Split,
                    },
                    http_filter: None,
                    ignore_ports: Default::default(),
                    skip: false,
                    config_patch: self.config_patch(),
                    run: RunSpec {
                        run_type: RunType::Exec,
                        command,
                        dir: Some(self.dir.display().to_string()),
                    },
                },
            }],
        })
    }

    fn run(&mut self) {
        let plan = match self.plan() {
            Ok(plan) => plan,
            Err(missing) => {
                self.problem = Some(format!("Missing {missing}."));
                return;
            }
        };
        match Launch::start(self.context.clone(), &plan, None) {
            Ok(launch) => {
                if let Some((command, _)) = self.command() {
                    history::record(
                        history::target_key(&self.target_spec()),
                        Some(self.dir.display().to_string()),
                        command,
                    );
                }
                self.problem = None;
                self.launch = Some(launch);
            }
            Err(error) => self.problem = Some(format!("Could not start mirrord: {error:#}")),
        }
    }

    fn open_picker(&mut self, pick: Pick) {
        let picker =
            match pick {
                Pick::Job => Picker::new(
                    "Job",
                    Job::VARIANTS
                        .iter()
                        .map(|job| job.label().to_owned())
                        .collect(),
                    Some(self.job.label().to_owned()),
                ),
                Pick::Target => {
                    let items = match self.discovery.read().ok().map(|discovery| {
                        match &discovery.candidates {
                            Fetch::Ready(candidates) => {
                                candidates.iter().map(Candidate::label).collect::<Vec<_>>()
                            }
                            _ => Vec::new(),
                        }
                    }) {
                        Some(items) if !items.is_empty() => items,
                        _ => return,
                    };
                    Picker::new(
                        "Target",
                        items,
                        self.target().map(|candidate| candidate.label()),
                    )
                }
                Pick::Command => {
                    let mut items: Vec<String> = Vec::new();
                    if let Some(past) = history::recall(&history::target_key(&self.target_spec())) {
                        items.extend(past.commands);
                    }
                    for command in &self.detected_commands {
                        if !items.contains(command) {
                            items.push(command.clone());
                        }
                    }
                    items.push(TYPE_OWN.to_owned());
                    Picker::new("Command", items, self.command().map(|(command, _)| command))
                }
            };
        self.picker = Some((pick, picker));
    }

    fn picked(&mut self, pick: Pick, choice: String) {
        match pick {
            Pick::Job => {
                if let Some(job) = Job::VARIANTS.iter().find(|job| job.label() == choice) {
                    self.job = *job;
                }
            }
            Pick::Target => self.target_choice = Some(choice),
            Pick::Command if choice == TYPE_OWN => {
                let mut input = TextInput::default();
                if let Some((command, _)) = self.command() {
                    input.paste(&command);
                }
                self.editor = Some(input);
            }
            Pick::Command => self.command_choice = Some(choice),
        }
        self.problem = None;
    }

    fn draw_review(&self, frame: &mut Frame, area: Rect) {
        // Read once and let go: the methods below take the lock again, and taking a read lock
        // while holding one deadlocks as soon as the discovery task is waiting to write.
        let (namespace, found) = match self.discovery.read() {
            Ok(discovery) => (
                discovery.namespace.clone(),
                match &discovery.candidates {
                    Fetch::Ready(candidates) => Found::Targets(candidates.len()),
                    Fetch::Failed(error) => Found::Failed(error.clone()),
                    Fetch::Disconnected => Found::Disconnected,
                    Fetch::Loading => Found::Loading,
                },
            ),
            Err(_) => (String::new(), Found::Loading),
        };
        let note = (!namespace.is_empty()).then(|| format!("in {namespace}"));
        let block = panel::block("run", None, note.as_deref(), None);
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let mut intro = Vec::new();
        if let Some(name) = self.names.first() {
            intro.push(Span::styled("Looks like ", theme::muted()));
            intro.push(Span::styled(name.name.clone(), value_style()));
            intro.push(Span::styled(format!(" ({})", name.source), theme::muted()));
        }
        if intro.is_empty() {
            intro.push(Span::styled(
                "No project files here; pick a target and a command.",
                theme::muted(),
            ));
        }

        let job = vec![
            Line::styled(self.job.label(), value_style()),
            Line::styled(self.job.about(), theme::muted()),
        ];
        let target = self.target_lines(&found);
        let command = self.command_lines();
        let plan = self.plan_lines();
        let footer = match (&self.problem, self.plan()) {
            (Some(problem), _) => Line::styled(problem.clone(), theme::error()),
            (None, Ok(_)) => Line::from_iter([
                Span::styled("\u{25cf} Ready", Style::default().fg(theme::MINT)),
                Span::styled(" - enter to start", Style::default().fg(theme::LAVENDER)),
            ]),
            (None, Err(missing)) => Line::styled(format!("Needs {missing}."), theme::warning()),
        };

        let plan_title = if self.show_yaml { "yaml" } else { "plan" };
        let cards: Vec<(&str, &str, Vec<Line<'static>>)> = vec![
            ("m", "job", job),
            ("t", "target", target),
            ("p", "command", command),
            ("y", plan_title, plan),
            ("b", "databases", self.database_lines()),
            (" ", "queues", self.queue_lines()),
        ];

        // Wide enough for two cards side by side; otherwise one card per row.
        let wide = inner.width >= WIDE_LAYOUT;
        let per_row = if wide { 2 } else { 1 };
        let cards_width = if wide {
            inner.width.saturating_sub(1) / 2
        } else {
            inner.width
        };
        let row_heights: Vec<u16> = cards
            .chunks(per_row)
            .map(|row| {
                row.iter()
                    .map(|(_, _, lines)| card_height(lines, cards_width))
                    .max()
                    .unwrap_or_default()
            })
            .collect();
        let needed: u16 = row_heights.iter().sum();
        // The intro line, the Ready line, and a blank line after each block.
        let room = inner.height.saturating_sub(4);
        let fits = needed <= room;

        // The Ready line follows the cards rather than sitting at the bottom of the panel.
        let [intro_area, cards_area, footer_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(needed.min(room)),
            Constraint::Length(1),
        ])
        .spacing(1)
        .areas(inner);
        frame.render_widget(Paragraph::new(Line::from(intro)), intro_area);
        frame.render_widget(Paragraph::new(footer), footer_area);

        if fits {
            let rows =
                Layout::vertical(row_heights.iter().map(|height| Constraint::Length(*height)))
                    .split(cards_area);
            let mut cards = cards.into_iter();
            for row in rows.iter() {
                let columns = Layout::horizontal(vec![Constraint::Fill(1); per_row])
                    .spacing(1)
                    .split(*row);
                for column in columns.iter() {
                    if let Some((key, name, lines)) = cards.next() {
                        card(frame, *column, key, name, lines);
                    }
                }
            }
        } else {
            compact(frame, cards_area, cards);
        }

        if let Some(checklist) = &self.checklist {
            checklist.draw(frame, area);
        }
        if let Some((_, picker)) = &self.picker {
            picker.draw(frame, area);
        }
    }

    fn target_lines(&self, found: &Found) -> Vec<Line<'static>> {
        if !self.job.needs_target() {
            return vec![
                Line::styled("none", value_style()),
                Line::styled("not needed for this job", theme::muted()),
            ];
        }
        match found {
            Found::Targets(count) => match self.target() {
                Some(candidate) => {
                    let mut lines = vec![
                        Line::styled(candidate.path(), value_style()),
                        Line::styled(
                            format!("{} \u{b7} {}", candidate.namespace, candidate.reason),
                            theme::muted(),
                        ),
                    ];
                    if *count > 1 {
                        lines.push(Line::styled(format!("+{} more", count - 1), theme::muted()));
                    }
                    lines
                }
                None => vec![Line::styled(
                    "Nothing to target here. Try :ns.",
                    theme::warning(),
                )],
            },
            Found::Failed(error) => vec![Line::styled(error.clone(), theme::error())],
            Found::Disconnected => vec![Line::styled("Waiting for the cluster...", theme::muted())],
            Found::Loading => vec![Line::styled("Looking for targets...", theme::muted())],
        }
    }

    fn command_lines(&self) -> Vec<Line<'static>> {
        if let Some(input) = &self.editor {
            let mut spans = vec![Span::styled("$ ", theme::title())];
            spans.extend(input.spans(value_style(), theme::title(), true));
            return vec![
                Line::from(spans),
                Line::styled("enter keep \u{b7} esc cancel", theme::muted()),
            ];
        }
        match self.command() {
            Some((command, source)) => vec![
                Line::styled(format!("$ {command}"), value_style()),
                Line::styled(source, theme::muted()),
            ],
            None => vec![Line::styled(
                "None found. Press p to type one.",
                theme::warning(),
            )],
        }
    }

    /// The plan card: a summary of what will run, or the YAML mirrord reads after `y`.
    fn plan_lines(&self) -> Vec<Line<'static>> {
        let plan = match self.plan() {
            Ok(plan) => plan,
            Err(missing) => return vec![Line::styled(format!("Needs {missing}."), theme::muted())],
        };
        if self.show_yaml {
            return plan
                .to_yaml()
                .unwrap_or_default()
                .lines()
                .map(|line| Line::styled(line.to_owned(), theme::muted()))
                .collect();
        }

        let row = |label: &'static str, value: String| {
            Line::from_iter([
                Span::styled(format!("{label:<8}"), theme::muted()),
                Span::styled(value, Style::default().fg(theme::LAVENDER)),
            ])
        };
        let target = match self.target_spec() {
            TargetSpec::Path { path, .. } => path,
            TargetSpec::Targetless => "none".to_owned(),
        };
        vec![
            row("target", target),
            row("traffic", self.job.traffic().to_owned()),
            row(
                "runs",
                self.command()
                    .map(|(command, _)| command)
                    .unwrap_or_default(),
            ),
            row(
                "in",
                suggest::compress_home(&self.dir.display().to_string()),
            ),
            row(
                "db",
                match self.branch.len() {
                    0 => "the originals".to_owned(),
                    count => format!("{count} branched"),
                },
            ),
        ]
    }
}

/// What inspecting the current target found.
#[derive(Default)]
struct Inspection {
    /// The [`Candidate::label`] of the target inspected.
    target: Option<String>,
    resources: Fetch<TargetResources>,
}

/// A database the user can choose to branch: a team preset, by id, or one found in the pod spec.
struct DbChoice {
    /// Identifies the choice in [`RunScreen::branch`].
    key: String,
    /// The checklist and card line, such as `pg · ORDERS_DATABASE_URL`.
    text: String,
    /// Where its credentials come from, or where the preset is defined.
    detail: String,
}

impl RunScreen {
    /// Inspects each target the view settles on, in the background: its databases and how it
    /// reaches them, the team's presets, its queues.
    async fn inspect_targets(
        mut context: Context,
        mut targets: watch::Receiver<Option<Candidate>>,
        inspection: Arc<RwLock<Inspection>>,
    ) {
        loop {
            let target = targets.borrow_and_update().clone();
            let client = context
                .client
                .borrow_and_update()
                .as_ref()
                .and_then(|result| result.as_ref().ok())
                .cloned();

            let state = match (&target, client) {
                (None, _) => Fetch::Ready(TargetResources::default()),
                (Some(_), None) => Fetch::Disconnected,
                (Some(candidate), Some(client)) => {
                    if let Ok(mut slot) = inspection.write() {
                        slot.target = Some(candidate.label());
                        slot.resources = Fetch::Loading;
                    }
                    context.request_redraw();
                    Fetch::Ready(cluster::inspect(&client, candidate).await)
                }
            };

            if let Ok(mut slot) = inspection.write() {
                slot.target = target.as_ref().map(Candidate::label);
                slot.resources = state;
            }
            context.request_redraw();

            tokio::select! {
                changed = targets.changed() => if changed.is_err() { break },
                changed = context.client.changed() => if changed.is_err() { break },
            }
        }
    }

    /// The databases the user can branch for the current target. The team's presets, when it
    /// has any, are the whole list: they are what the team set up for branching this workload.
    fn db_choices(&self) -> Vec<DbChoice> {
        let Ok(inspection) = self.inspection.read() else {
            return Vec::new();
        };
        let Fetch::Ready(resources) = &inspection.resources else {
            return Vec::new();
        };
        if !resources.presets.is_empty() {
            return resources
                .presets
                .iter()
                .map(|preset| DbChoice {
                    key: format!("preset:{}", preset.id),
                    text: format!("{} \u{b7} {}", preset.engine, preset.id),
                    detail: "team preset (MirrordSplitConfig)".to_owned(),
                })
                .collect();
        }
        resources
            .databases
            .iter()
            .filter(|database| database.engine.branchable())
            .map(|database| DbChoice {
                key: format!("env:{}", database.label()),
                text: format!("{} \u{b7} {}", database.engine.name(), database.label()),
                detail: database.credentials(),
            })
            .collect()
    }

    /// The config patch that branches the chosen databases, or `None` when none are chosen.
    fn config_patch(&self) -> Option<Value> {
        if self.branch.is_empty() {
            return None;
        }
        let inspection = self.inspection.read().ok()?;
        let Fetch::Ready(resources) = &inspection.resources else {
            return None;
        };

        let branches: Vec<Value> = if resources.presets.is_empty() {
            resources
                .databases
                .iter()
                .filter(|database| self.branch.contains(&format!("env:{}", database.label())))
                .map(|database| database.branch_config())
                .collect()
        } else {
            // Presets are branched by id: the operator fills in the rest from the split config.
            resources
                .presets
                .iter()
                .filter(|preset| self.branch.contains(&format!("preset:{}", preset.id)))
                .map(|preset| Value::String(preset.id.clone()))
                .collect()
        };
        (!branches.is_empty()).then(|| json!({ "feature": { "db_branches": branches } }))
    }

    fn open_checklist(&mut self) {
        let choices = self.db_choices();
        if choices.is_empty() {
            return;
        }
        self.checklist = Some(Checklist {
            items: choices
                .into_iter()
                .map(|choice| (choice.key, choice.text, choice.detail))
                .collect(),
            chosen: self.branch.clone(),
            cursor: 0,
        });
    }

    fn database_lines(&self) -> Vec<Line<'static>> {
        if !self.job.needs_target() {
            return vec![Line::styled("none without a target", theme::muted())];
        }
        let resources = match self
            .inspection
            .read()
            .as_deref()
            .map(|inspection| &inspection.resources)
        {
            Ok(Fetch::Ready(resources)) => resources.clone(),
            Ok(Fetch::Loading) => {
                return vec![Line::styled("Reading the pod spec...", theme::muted())];
            }
            Ok(Fetch::Disconnected) => {
                return vec![Line::styled("Waiting for the cluster...", theme::muted())];
            }
            Ok(Fetch::Failed(error)) => return vec![Line::styled(error.clone(), theme::error())],
            Err(_) => return Vec::new(),
        };

        let mut lines = Vec::new();
        let choices = self.db_choices();
        for choice in &choices {
            let chosen = self.branch.contains(&choice.key);
            lines.push(Line::from_iter([
                Span::styled(
                    if chosen { "[x] " } else { "[ ] " },
                    Style::default().fg(if chosen { theme::MINT } else { theme::MUTED }),
                ),
                Span::styled(choice.text.clone(), value_style()),
            ]));
            lines.push(Line::styled(
                format!("    {}", choice.detail),
                theme::muted(),
            ));
        }
        if resources.presets.is_empty() {
            for database in resources
                .databases
                .iter()
                .filter(|database| !database.engine.branchable())
            {
                lines.push(Line::styled(
                    format!(
                        "{} \u{b7} {}: branch it in :config",
                        database.engine.name(),
                        database.label()
                    ),
                    theme::muted(),
                ));
            }
            for variable in &resources.unidentified {
                lines.push(Line::styled(
                    format!("{variable}: engine unknown, branch it in :config"),
                    theme::muted(),
                ));
            }
        }
        for note in &resources.notes {
            lines.push(Line::styled(note.clone(), theme::warning()));
        }

        if choices.is_empty() {
            if lines.is_empty() {
                lines.push(Line::styled(
                    "No databases found in the pod spec.",
                    theme::muted(),
                ));
            }
        } else {
            let summary = match self.branch.len() {
                0 => "not branched - b to choose".to_owned(),
                count => format!("{count} branched for this session - b to change"),
            };
            lines.insert(0, Line::styled(summary, theme::muted()));
        }
        lines
    }

    fn queue_lines(&self) -> Vec<Line<'static>> {
        if !self.job.needs_target() {
            return vec![Line::styled("none without a target", theme::muted())];
        }
        let queues = match self
            .inspection
            .read()
            .as_deref()
            .map(|inspection| &inspection.resources)
        {
            Ok(Fetch::Ready(resources)) => resources.queues.clone(),
            Ok(Fetch::Loading) => {
                return vec![Line::styled("Asking the operator...", theme::muted())];
            }
            _ => return vec![Line::styled("Waiting for the cluster...", theme::muted())],
        };
        if queues.is_empty() {
            return vec![Line::styled(
                "None registered with the operator.",
                theme::muted(),
            )];
        }
        let mut lines: Vec<Line> = queues
            .iter()
            .map(|queue| {
                Line::from_iter([
                    Span::styled(queue.id.clone(), value_style()),
                    Span::styled(format!(" \u{b7} {}", queue.kind), theme::muted()),
                ])
            })
            .collect();
        lines.push(Line::styled(
            match self.job {
                Job::OnlyMine => "messages with your session key come to you",
                Job::TakeOver => "the copy reads them; nothing is split",
                Job::NoTraffic => "not split without a target",
            },
            theme::muted(),
        ));
        lines
    }
}

/// What a key press in the checklist decided.
enum ChecklistOutcome {
    Open,
    Cancelled,
    Done(BTreeSet<String>),
}

/// A dialog of databases, each switched on or off for branching.
struct Checklist {
    /// Key, text and detail of each database.
    items: Vec<(String, String, String)>,
    chosen: BTreeSet<String>,
    cursor: usize,
}

impl Checklist {
    fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> ChecklistOutcome {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.cursor = (self.cursor + 1).min(self.items.len().saturating_sub(1));
            }
            KeyCode::Char(' ') => {
                if let Some((key, _, _)) = self.items.get(self.cursor)
                    && !self.chosen.remove(key)
                {
                    self.chosen.insert(key.clone());
                }
            }
            KeyCode::Char('a') => {
                if self.chosen.len() == self.items.len() {
                    self.chosen.clear();
                } else {
                    self.chosen = self.items.iter().map(|(key, _, _)| key.clone()).collect();
                }
            }
            KeyCode::Enter => return ChecklistOutcome::Done(self.chosen.clone()),
            KeyCode::Esc => return ChecklistOutcome::Cancelled,
            _ => {}
        }
        ChecklistOutcome::Open
    }

    fn draw(&self, frame: &mut Frame, area: Rect) {
        let mut lines = Vec::new();
        for (index, (key, text, detail)) in self.items.iter().enumerate() {
            let chosen = self.chosen.contains(key);
            let mut line = Line::from_iter([
                Span::styled(
                    if index == self.cursor {
                        "\u{203a} "
                    } else {
                        "  "
                    },
                    theme::title(),
                ),
                Span::styled(
                    if chosen { "[x] " } else { "[ ] " },
                    Style::default().fg(if chosen { theme::MINT } else { theme::MUTED }),
                ),
                Span::styled(text.clone(), value_style()),
            ]);
            if index == self.cursor {
                line = line.style(theme::selection());
            }
            lines.push(line);
            lines.push(Line::styled(format!("      {detail}"), theme::muted()));
        }
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "Each one gets its own copy for this session; the originals are not touched.",
            theme::muted(),
        ));

        let height = (lines.len() as u16 + 2).min(area.height);
        let inner = crate::helpers::dialog(
            frame,
            crate::helpers::centered(area, Constraint::Length(72), Constraint::Length(height)),
            "Branch databases",
        );
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    }
}

/// Namespaces that never hold the user's services.
const SYSTEM_NAMESPACES: &[&str] = &["kube-system", "kube-public", "kube-node-lease", "mirrord"];

/// Most namespaces searched when none is picked. Each one costs a request per target kind, so a
/// cluster with more than this is searched in its default namespace only, and `:ns` picks
/// another.
const MAX_SEARCHED_NAMESPACES: usize = 30;

/// The namespaces to look for the project's target in when the user has not picked one: every
/// namespace but the system ones, or only the client's default namespace when there are too many
/// or they cannot be listed.
async fn searched_namespaces(client: &kube::Client) -> Vec<String> {
    let fallback = || vec![client.default_namespace().to_owned()];
    let Ok(list) = kube::Api::<Namespace>::all(client.clone())
        .list(&Default::default())
        .await
    else {
        return fallback();
    };
    let namespaces: Vec<String> = list
        .items
        .into_iter()
        .filter_map(|namespace| namespace.metadata.name)
        .filter(|name| !SYSTEM_NAMESPACES.contains(&name.as_str()))
        .collect();
    if namespaces.is_empty() || namespaces.len() > MAX_SEARCHED_NAMESPACES {
        fallback()
    } else {
        namespaces
    }
}

fn value_style() -> Style {
    Style::default()
        .fg(theme::LAVENDER)
        .add_modifier(Modifier::BOLD)
}

/// Inner width at which the cards sit two to a row.
const WIDE_LAYOUT: u16 = 90;

/// How tall a card holding `lines` is at `width`, borders included, so cards are as tall as
/// their content rather than splitting the screen evenly.
fn card_height(lines: &[Line], width: u16) -> u16 {
    // The border and the one-column padding on each side.
    let inner = usize::from(width.saturating_sub(4)).max(1);
    let rows: usize = lines
        .iter()
        .map(|line| line.width().div_ceil(inner).max(1))
        .sum();
    u16::try_from(rows + 2).unwrap_or(u16::MAX)
}

/// Width of the label column in the compact layout: `<b> DATABASES` and a space.
const COMPACT_LABEL: usize = 14;

/// The cards as a plain list, for terminals too short for boxes: one row per card with its key,
/// name and first line, then its other lines indented under it, card by card, for as long as
/// there is room. Every card keeps at least its first line.
fn compact(frame: &mut Frame, area: Rect, cards: Vec<(&str, &str, Vec<Line<'static>>)>) {
    let width = usize::from(area.width);
    let value_width = width.saturating_sub(COMPACT_LABEL);
    // Lines left for details once every card has its first line.
    let mut room = usize::from(area.height).saturating_sub(cards.len());

    let fit = |line: &Line<'static>| -> Span<'static> {
        let text: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        let style = line.spans.last().map(|span| span.style).unwrap_or_default();
        Span::styled(ellipsize(&text, value_width), style)
    };

    let mut rows: Vec<Line> = Vec::new();
    for (key, name, lines) in cards {
        let mut lines = lines.into_iter();
        let key = if key.trim().is_empty() {
            "    ".to_owned()
        } else {
            format!("<{key}> ")
        };
        let mut first = vec![
            Span::styled(
                key,
                Style::default()
                    .fg(theme::INDIGO)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{:<width$}", name.to_uppercase(), width = COMPACT_LABEL - 4),
                theme::table_header(),
            ),
        ];
        if let Some(line) = lines.next() {
            first.push(fit(&line));
        }
        rows.push(Line::from(first));
        for line in lines {
            if room == 0 {
                break;
            }
            room -= 1;
            rows.push(Line::from(vec![
                Span::raw(" ".repeat(COMPACT_LABEL)),
                fit(&line),
            ]));
        }
    }
    frame.render_widget(Paragraph::new(rows), area);
}

/// One review card: a rounded box titled with the key that changes it and what it is.
fn card(frame: &mut Frame, area: Rect, key: &str, name: &str, lines: Vec<Line<'static>>) {
    let mut title = vec![Span::raw(" ")];
    if !key.trim().is_empty() {
        title.push(Span::styled(
            format!("<{key}> "),
            Style::default()
                .fg(theme::INDIGO)
                .add_modifier(Modifier::BOLD),
        ));
    }
    title.push(Span::styled(name.to_uppercase(), theme::table_header()));
    title.push(Span::raw(" "));
    let title = Line::from(title);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::BORDER_DIM))
        .padding(Padding::horizontal(1))
        .title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

impl RunScreen {
    /// Whether the TUI was started inside a project: a manifest names it, or the folder has a
    /// command to run. The application opens on this view then, since starting a session is
    /// what someone in a project folder most likely came to do.
    pub fn in_project(&self) -> bool {
        !self.detected_commands.is_empty()
            || self
                .names
                .iter()
                .any(|name| name.source != detect::FOLDER_NAME_SOURCE)
    }
}

impl Screen for RunScreen {
    fn new(context: Context) -> Self {
        let dir = std::env::current_dir().unwrap_or_default();
        let names = detect::project_names(&dir);
        let detected_commands = suggest::commands_in(Some(&dir.display().to_string()));
        let discovery = Arc::new(RwLock::new(Discovery::default()));
        let inspection = Arc::new(RwLock::new(Inspection::default()));
        let (inspect, inspect_rx) = watch::channel(None);
        tokio::spawn(Self::inspect_targets(
            context.clone(),
            inspect_rx,
            inspection.clone(),
        ));

        tokio::spawn(Self::discover(
            context.clone(),
            dir.clone(),
            names.clone(),
            discovery.clone(),
        ));

        Self {
            context,
            dir,
            names,
            detected_commands,
            discovery,
            job: Job::OnlyMine,
            target_choice: None,
            command_choice: None,
            editor: None,
            picker: None,
            launch: None,
            show_yaml: false,
            inspection,
            inspect,
            branch: BTreeSet::new(),
            checklist: None,
            problem: None,
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        if let Some(launch) = &mut self.launch {
            launch.draw(frame, area, true);
            return;
        }

        // A different target has different databases: inspect it, and drop choices made for the
        // previous one.
        let target = self.target().filter(|_| self.job.needs_target());
        let changed = self.inspect.send_if_modified(|current| {
            let differs =
                current.as_ref().map(Candidate::label) != target.as_ref().map(Candidate::label);
            if differs {
                *current = target.clone();
            }
            differs
        });
        if changed {
            self.branch.clear();
        }

        self.draw_review(frame, area);
    }

    fn handle_event(&mut self, event: Event) -> ControlFlow<(), Event> {
        if let Some(launch) = &mut self.launch {
            if let Event::Key(key) = event
                && launch.handle_key(key)
            {
                self.launch = None;
            }
            return ControlFlow::Break(());
        }

        if let Some(input) = &mut self.editor {
            match &event {
                Event::Paste(text) => input.paste(text),
                Event::Key(key) => match key.code {
                    KeyCode::Enter => {
                        let command = input.value().trim().to_owned();
                        self.command_choice = (!command.is_empty()).then_some(command);
                        self.editor = None;
                    }
                    KeyCode::Esc => self.editor = None,
                    _ => {
                        input.handle_key(key);
                    }
                },
                _ => {}
            }
            return ControlFlow::Break(());
        }

        if let Some(checklist) = &mut self.checklist {
            if let Event::Key(key) = event {
                match checklist.handle_key(key) {
                    ChecklistOutcome::Open => {}
                    ChecklistOutcome::Cancelled => self.checklist = None,
                    ChecklistOutcome::Done(chosen) => {
                        self.branch = chosen;
                        self.checklist = None;
                    }
                }
            }
            return ControlFlow::Break(());
        }

        if let Some((_, picker)) = &mut self.picker {
            if let Event::Key(key) = event {
                match picker.handle_key(key) {
                    PickerOutcome::Open => {}
                    PickerOutcome::Cancelled => self.picker = None,
                    PickerOutcome::Picked(choice) => {
                        if let Some((pick, _)) = self.picker.take() {
                            self.picked(pick, choice);
                        }
                    }
                }
            }
            return ControlFlow::Break(());
        }

        let Event::Key(key) = &event else {
            return ControlFlow::Continue(event);
        };
        match key.code {
            KeyCode::Enter => self.run(),
            KeyCode::Char('m') => self.open_picker(Pick::Job),
            KeyCode::Char('t') => self.open_picker(Pick::Target),
            KeyCode::Char('p') => self.open_picker(Pick::Command),
            KeyCode::Char('y') => self.show_yaml = !self.show_yaml,
            KeyCode::Char('b') => self.open_checklist(),
            _ => return ControlFlow::Continue(event),
        }
        ControlFlow::Break(())
    }

    fn hints(&self) -> Vec<Hint> {
        if self.launch.is_some() {
            return vec![
                Hint::new("x", "stop"),
                Hint::new("\u{2191}/\u{2193}", "scroll"),
                Hint::new("G", "follow"),
                Hint::new("esc", "back, once stopped"),
            ];
        }
        if self.editor.is_some() {
            return vec![Hint::new("enter", "keep"), Hint::new("esc", "cancel")];
        }
        if self.picker.is_some() {
            return vec![Hint::new("enter", "pick"), Hint::new("esc", "cancel")];
        }
        if self.checklist.is_some() {
            return vec![
                Hint::new("space", "branch or not"),
                Hint::new("a", "all or none"),
                Hint::new("enter", "done"),
                Hint::new("esc", "cancel"),
            ];
        }
        vec![
            Hint::new("enter", "run"),
            Hint::new("m", "job"),
            Hint::new("t", "target"),
            Hint::new("p", "command"),
            Hint::new("y", if self.show_yaml { "summary" } else { "yaml" }),
            Hint::new("b", "databases"),
        ]
    }
}
