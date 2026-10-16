//! The Run view: start a mirrord session in three short steps, each a list to pick from.
//!
//! 1. **Folders**: which folders to run, from anywhere on disk. The projects found under the home
//!    folder and the folders run from before are listed first; the disk can be browsed from there.
//! 2. **Targets**: what each folder runs against. Every target in the cluster is listed, the ones
//!    that look like a picked folder on top with a sign of how sure the guess is (`★` sure, `☆`
//!    maybe), and the sure ones already ticked. Each ticked target is matched to the folder it fits
//!    best.
//! 3. **Config**: how mirrord runs them, chosen from ready-made configs: made on the spot from the
//!    picks (only my requests, with databases branched, take over, no traffic) or found in the
//!    folders. Typing searches them by what they do.
//!
//! Every step works the same way: `Space` or a click picks and unpicks, typing searches, `Enter`
//! goes on, `Esc` goes back, and a tray under the list shows what is picked with a `✕` to undo it.
//! The session runs through `mirrord up`, so what the view does is exactly what the CLI would do
//! with the plan it shows.

use std::{
    cmp::{Ordering, Reverse},
    collections::{BTreeMap, BTreeSet},
    ops::ControlFlow,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
use futures_util::future::join_all;
use k8s_openapi::api::core::v1::Namespace;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Margin, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Padding, Paragraph, Wrap},
};
use serde_json::{Value, json};
use tokio::sync::watch;

use crate::{
    context::Context,
    helpers::{centered, dialog, ellipsize},
    kit::{
        hints::{self, Hint},
        panel,
        select_list::{Choice, ListEvent, Picks, SelectList},
        state_view::Fetch,
        text_input::TextInput,
        tray::Tray,
    },
    screens::{
        Screen,
        targets::{
            browser::{self, TargetItem, TargetKind},
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
mod configs;
mod databases;
pub(crate) mod detect;
mod folders;
pub(crate) mod projects;

use cluster::TargetResources;
use configs::{ConfigOption, Setup};
use detect::{Candidate, ProjectName};
use folders::FolderStep;

/// What the session does with the targets' traffic.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Job {
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
    fn needs_target(self) -> bool {
        self != Self::NoTraffic
    }

    fn traffic(self) -> &'static str {
        match self {
            Self::OnlyMine => "only yours (session key)",
            Self::TakeOver => "all, original scaled down",
            Self::NoTraffic => "none",
        }
    }

    /// Safe on a shared cluster, affects everyone, or touches no traffic at all.
    fn tone(self) -> Color {
        match self {
            Self::OnlyMine => theme::MINT,
            Self::TakeOver => theme::CORAL,
            Self::NoTraffic => theme::GREY,
        }
    }
}

/// The three steps, in order.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum Step {
    Folders,
    Targets,
    Config,
}

impl Step {
    const ALL: [Step; 3] = [Step::Folders, Step::Targets, Step::Config];

    fn name(self) -> &'static str {
        match self {
            Self::Folders => "Folders",
            Self::Targets => "Targets",
            Self::Config => "Config",
        }
    }

    fn question(self) -> &'static str {
        match self {
            Self::Folders => "Which folders do you want to run?",
            Self::Targets => "What should they run against?",
            Self::Config => "How should mirrord run them?",
        }
    }

    fn help(self) -> &'static str {
        match self {
            Self::Folders => "Pick any number, from anywhere on disk.",
            Self::Targets => {
                "\u{2605} sure match  \u{2606} might be it. Each pick goes to the folder it fits."
            }
            Self::Config => "Made from your picks, or found in your folders. Type to search.",
        }
    }

    fn previous(self) -> Option<Self> {
        match self {
            Self::Folders => None,
            Self::Targets => Some(Self::Folders),
            Self::Config => Some(Self::Targets),
        }
    }

    fn next(self) -> Option<Self> {
        match self {
            Self::Folders => Some(Self::Targets),
            Self::Targets => Some(Self::Config),
            Self::Config => None,
        }
    }
}

/// Every target found in the searched namespaces: namespace, kind, and the targets of that kind.
type Listings = Vec<(String, TargetKind, Vec<TargetItem>)>;

/// What the background discovery found in the cluster.
#[derive(Default)]
struct Discovery {
    namespace: String,
    targets: Fetch<Listings>,
    /// Goes up with every new result, so each folder knows when to rank its targets again.
    round: u64,
}

/// What inspecting each target found, by [`Candidate::label`].
type Inspections = BTreeMap<String, Fetch<TargetResources>>;

/// One picked folder and what the view worked out for it.
struct Service {
    dir: PathBuf,
    names: Vec<ProjectName>,
    /// Commands the project files point to, best guess first.
    detected_commands: Vec<String>,
    /// Every target, best match for this folder first.
    candidates: Vec<Candidate>,
    /// The [`Discovery::round`] `candidates` were ranked from.
    ranked_round: Option<u64>,
    /// The command the user picked or typed; `None` follows history, then detection.
    command_choice: Option<String>,
}

impl Service {
    fn new(dir: PathBuf) -> Self {
        Self {
            names: detect::project_names(&dir),
            detected_commands: suggest::commands_in(Some(&dir.display().to_string())),
            dir,
            candidates: Vec::new(),
            ranked_round: None,
            command_choice: None,
        }
    }

    /// What the folder is called in the view and in the plan: its best project name.
    fn name(&self) -> String {
        self.names
            .first()
            .map(|name| name.name.clone())
            .unwrap_or_else(|| "local".to_owned())
    }

    /// Whether the folder is a project: a manifest names it, or it has a command to run.
    fn is_project(&self) -> bool {
        !self.detected_commands.is_empty()
            || self
                .names
                .iter()
                .any(|name| name.source != detect::FOLDER_NAME_SOURCE)
    }

    fn dir_text(&self) -> String {
        self.dir.display().to_string()
    }
}

/// A picked target and the folder it runs for.
#[derive(Clone)]
struct PickedTarget {
    candidate: Candidate,
    dir: PathBuf,
}

/// A pick-one dialog open over the view, and what it picks.
enum Pick {
    /// Which folder's command to change.
    Folder,
    /// The command for the folder at this path.
    Command(PathBuf),
}

/// The entry of the command picker that opens the editor instead of picking a command.
const TYPE_OWN: &str = "type my own command...";

/// Where a command came from when it was run from the folder before.
const RAN_LAST_TIME: &str = "ran last time";

/// Inner width at which the details pane sits next to the list; narrower, it goes under it.
const WIDE_LAYOUT: u16 = 90;

/// Longest a folder's path gets in the config step's preview before it is cut.
const MAX_PREVIEW_PATH: usize = 28;

/// Height of the body at which the details pane fits under the list on a narrow terminal.
const TALL_LAYOUT: u16 = 16;

pub struct RunScreen {
    context: Context,
    step: Step,
    /// The picked folders, in the order they were picked.
    services: Vec<Service>,
    folders: FolderStep,
    targets: SelectList,
    picked: Vec<PickedTarget>,
    /// Folders whose sure match has been ticked for them once. The user's own unpicking is never
    /// undone by ticking it again.
    auto_picked: BTreeSet<PathBuf>,
    configs: SelectList,
    /// The config picked, by [`ConfigOption::id`]; `None` follows the first one offered.
    config_choice: Option<String>,
    discovery: Arc<RwLock<Discovery>>,
    /// Projects found under the home folder, once the background search is done.
    found: Arc<RwLock<Option<Vec<PathBuf>>>>,
    inspection: Arc<RwLock<Inspections>>,
    /// The targets to inspect, handed to the background inspection whenever they change.
    inspect: watch::Sender<Vec<Candidate>>,
    tray: Tray,
    /// Where each step's name landed in the step bar, for clicks.
    step_hits: Vec<(Rect, Step)>,
    /// `Ctrl+Y`: the config step shows the YAML mirrord runs instead of its summary.
    show_yaml: bool,
    picker: Option<(Pick, Picker)>,
    /// The command being typed, and the folder it is for.
    editor: Option<(PathBuf, TextInput)>,
    launch: Option<Launch>,
    /// Why the last move or run could not happen.
    problem: Option<String>,
}

impl RunScreen {
    /// Lists the targets in the scope's namespace, again whenever the connection or the scope
    /// changes. Each folder ranks them against its own names.
    async fn discover(mut context: Context, discovery: Arc<RwLock<Discovery>>) {
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
                        Fetch::Ready(listings)
                    };
                    (label, state)
                }
            };

            if let Ok(mut slot) = discovery.write() {
                slot.namespace = state.0;
                slot.targets = state.1;
                slot.round += 1;
            }
            context.request_redraw();

            tokio::select! {
                changed = context.client.changed() => if changed.is_err() { break },
                changed = context.scope.changed() => if changed.is_err() { break },
            }
        }
    }

    /// Reads each wanted target from the cluster once, in the background: its databases and how
    /// it reaches them, the team's presets, its queues.
    async fn inspect_targets(
        mut context: Context,
        mut wanted: watch::Receiver<Vec<Candidate>>,
        inspection: Arc<RwLock<Inspections>>,
    ) {
        loop {
            let targets = wanted.borrow_and_update().clone();
            let client = context
                .client
                .borrow_and_update()
                .as_ref()
                .and_then(|result| result.as_ref().ok())
                .cloned();

            if let Some(client) = client {
                for candidate in targets {
                    let label = candidate.label();
                    let known = inspection
                        .read()
                        .map(|found| found.contains_key(&label))
                        .unwrap_or(true);
                    if known {
                        continue;
                    }
                    if let Ok(mut slot) = inspection.write() {
                        slot.insert(label.clone(), Fetch::Loading);
                    }
                    context.request_redraw();
                    let found = cluster::inspect(&client, &candidate).await;
                    if let Ok(mut slot) = inspection.write() {
                        slot.insert(label, Fetch::Ready(found));
                    }
                    context.request_redraw();
                }
            }

            tokio::select! {
                changed = wanted.changed() => if changed.is_err() { break },
                changed = context.client.changed() => if changed.is_err() { break },
            }
        }
    }

    /// Ranks the targets for every folder that has not seen the latest discovery result.
    fn rank_services(&mut self) {
        let Ok(discovery) = self.discovery.read() else {
            return;
        };
        for service in &mut self.services {
            if service.ranked_round == Some(discovery.round) {
                continue;
            }
            service.ranked_round = Some(discovery.round);
            service.candidates = match &discovery.targets {
                Fetch::Ready(listings) => {
                    let here = service.dir_text();
                    let ran_before = |namespace: &str, path: &str| {
                        let target = TargetSpec::Path {
                            path: path.to_owned(),
                            namespace: Some(namespace.to_owned()),
                        };
                        history::recall(&history::target_key(&target))
                            .is_some_and(|past| past.dir.as_deref() == Some(here.as_str()))
                    };
                    detect::rank(listings, &service.names, ran_before)
                }
                _ => Vec::new(),
            };
        }
    }

    /// Ticks each folder's sure match once its targets are known, unless it already has a target.
    fn auto_pick_targets(&mut self) {
        let mut picks = Vec::new();
        for service in &self.services {
            // Not listed yet (or nothing there): try again on a later round.
            if self.auto_picked.contains(&service.dir) || service.candidates.is_empty() {
                continue;
            }
            picks.push(service.dir.clone());
        }
        for dir in picks {
            self.auto_picked.insert(dir.clone());
            if self.target_of(&dir).is_some() {
                continue;
            }
            let sure = self
                .service(&dir)
                .and_then(|service| service.candidates.first())
                .filter(|candidate| candidate.is_sure())
                .filter(|candidate| {
                    !self
                        .picked
                        .iter()
                        .any(|picked| picked.candidate.label() == candidate.label())
                })
                .cloned();
            if let Some(candidate) = sure {
                self.picked.push(PickedTarget { candidate, dir });
            }
        }
    }

    fn service(&self, dir: &PathBuf) -> Option<&Service> {
        self.services.iter().find(|service| &service.dir == dir)
    }

    fn target_of(&self, dir: &PathBuf) -> Option<&Candidate> {
        self.picked
            .iter()
            .find(|picked| &picked.dir == dir)
            .map(|picked| &picked.candidate)
    }

    fn toggle_folder(&mut self, id: &str) {
        let dir = PathBuf::from(id);
        match self.services.iter().position(|service| service.dir == dir) {
            Some(index) => {
                self.services.remove(index);
                self.picked.retain(|picked| picked.dir != dir);
            }
            None => self.services.push(Service::new(dir)),
        }
        self.problem = None;
    }

    /// Ticks or unticks a target. A ticked one goes to the folder it fits best that has no target
    /// yet, or replaces the target of the folder it fits best when every folder has one.
    fn toggle_target(&mut self, label: &str) {
        if let Some(index) = self
            .picked
            .iter()
            .position(|picked| picked.candidate.label() == label)
        {
            self.picked.remove(index);
            return;
        }
        let mut fits: Vec<(u32, usize, Candidate)> = self
            .services
            .iter()
            .enumerate()
            .filter_map(|(index, service)| {
                let candidate = service
                    .candidates
                    .iter()
                    .find(|candidate| candidate.label() == label)?;
                Some((candidate.score, index, candidate.clone()))
            })
            .collect();
        fits.sort_by(|left, right| right.0.cmp(&left.0).then(left.1.cmp(&right.1)));
        let free = fits.iter().find(|(_, index, _)| {
            self.services
                .get(*index)
                .is_some_and(|service| self.target_of(&service.dir).is_none())
        });
        let Some((_, index, candidate)) = free.or(fits.first()).cloned() else {
            return;
        };
        let Some(dir) = self.services.get(index).map(|service| service.dir.clone()) else {
            return;
        };
        self.picked.retain(|picked| picked.dir != dir);
        self.picked.push(PickedTarget { candidate, dir });
        self.problem = None;
    }

    /// `Ctrl+F`: moves the ticked target under the cursor to the next folder, swapping with that
    /// folder's target if it has one.
    fn cycle_target_folder(&mut self) {
        let Some(label) = self.targets.focused().map(|choice| choice.id.clone()) else {
            return;
        };
        let Some(at) = self
            .picked
            .iter()
            .position(|picked| picked.candidate.label() == label)
        else {
            return;
        };
        let from = self.picked[at].dir.clone();
        let Some(current) = self.services.iter().position(|service| service.dir == from) else {
            return;
        };
        let Some(next) = self
            .services
            .get((current + 1) % self.services.len())
            .map(|service| service.dir.clone())
        else {
            return;
        };
        for picked in &mut self.picked {
            if picked.dir == next {
                picked.dir = from.clone();
            }
        }
        self.picked[at].dir = next;
    }

    /// The command a folder runs, and where it came from.
    fn command(&self, service: &Service, job: Job) -> Option<(String, String)> {
        if let Some(command) = &service.command_choice {
            return Some((command.clone(), "your pick".to_owned()));
        }
        let past = history::recall(&history::target_key(&self.target_spec(service, job)));
        if let Some(command) = past
            .filter(|past| past.dir.as_deref() == Some(service.dir_text().as_str()))
            .and_then(|past| past.commands.into_iter().next())
        {
            return Some((command, RAN_LAST_TIME.to_owned()));
        }
        service
            .detected_commands
            .first()
            .map(|command| (command.clone(), "detected - check it".to_owned()))
    }

    fn target_spec(&self, service: &Service, job: Job) -> TargetSpec {
        match self.target_of(&service.dir).filter(|_| job.needs_target()) {
            Some(candidate) => TargetSpec::Path {
                path: candidate.path(),
                namespace: Some(candidate.namespace.clone()),
            },
            None => TargetSpec::Targetless,
        }
    }

    /// What inspecting a target found, copied out of the lock.
    fn resources(&self, candidate: &Candidate) -> Option<TargetResources> {
        match self.inspection.read().ok()?.get(&candidate.label()) {
            Some(Fetch::Ready(resources)) => Some(resources.clone()),
            _ => None,
        }
    }

    fn config_options(&self) -> Vec<ConfigOption> {
        let dirs: Vec<PathBuf> = self
            .services
            .iter()
            .map(|service| service.dir.clone())
            .collect();
        let resources: Vec<TargetResources> = self
            .picked
            .iter()
            .filter_map(|picked| self.resources(&picked.candidate))
            .collect();
        configs::options(&dirs, !self.picked.is_empty(), &resources)
    }

    /// The config in effect: the user's pick while it is still offered, else the first one.
    fn config(&self) -> Option<ConfigOption> {
        let options = self.config_options();
        self.config_choice
            .as_ref()
            .and_then(|id| options.iter().find(|option| &option.id == id).cloned())
            .or_else(|| options.into_iter().next())
    }

    /// The job a config gives a folder: a file config runs like "only my requests", and a folder
    /// without a target always runs with no traffic.
    fn job_for(&self, config: &ConfigOption, service: &Service) -> Job {
        let job = match &config.setup {
            Setup::Made { job, .. } => *job,
            Setup::File { .. } => Job::OnlyMine,
        };
        if job.needs_target() && self.target_of(&service.dir).is_none() {
            Job::NoTraffic
        } else {
            job
        }
    }

    /// The config patch a folder runs with: its databases branched, or a file's content.
    fn config_patch(&self, config: &ConfigOption, service: &Service) -> Option<Value> {
        match &config.setup {
            Setup::File { dir, content } if *dir == service.dir => {
                Some(Value::Object(content.clone()))
            }
            Setup::Made {
                branch_databases: true,
                ..
            } => {
                let resources = self.resources(self.target_of(&service.dir)?)?;
                let branches: Vec<Value> = if resources.presets.is_empty() {
                    resources
                        .databases
                        .iter()
                        .filter(|database| database.engine.branchable())
                        .map(|database| database.branch_config())
                        .collect()
                } else {
                    // Presets are branched by id: the operator fills in the rest.
                    resources
                        .presets
                        .iter()
                        .map(|preset| Value::String(preset.id.clone()))
                        .collect()
                };
                (!branches.is_empty()).then(|| json!({ "feature": { "db_branches": branches } }))
            }
            _ => None,
        }
    }

    /// The plan `mirrord up` runs, with one service per folder, or what it still needs.
    fn plan(&self) -> Result<UpFile, String> {
        if self.services.is_empty() {
            return Err("a folder to run".to_owned());
        }
        let config = self.config().ok_or("a config")?;
        let mut names = BTreeSet::new();
        let mut services = Vec::new();
        for service in &self.services {
            let label = service.name();
            let job = self.job_for(&config, service);
            let (command, _) = self
                .command(service, job)
                .ok_or_else(|| format!("a command for {label} - ctrl+e"))?;
            let command = split_command(&command)
                .map_err(|_| format!("a command that parses for {label}"))?;

            // Two folders on the same target, or with the same name, still need their own names.
            let base = self
                .target_of(&service.dir)
                .filter(|_| job.needs_target())
                .map(|candidate| candidate.name.clone())
                .unwrap_or(label);
            let mut name = base.clone();
            for suffix in 2.. {
                if names.insert(name.clone()) {
                    break;
                }
                name = format!("{base}-{suffix}");
            }

            services.push(ServiceEntry {
                name,
                spec: ServiceSpec {
                    target: self.target_spec(service, job),
                    default_mode: match job {
                        Job::TakeOver => ServiceMode::Replace,
                        Job::OnlyMine | Job::NoTraffic => ServiceMode::Split,
                    },
                    http_filter: None,
                    ignore_ports: Default::default(),
                    skip: false,
                    config_patch: self.config_patch(&config, service),
                    run: RunSpec {
                        run_type: RunType::Exec,
                        command,
                        dir: Some(service.dir_text()),
                    },
                },
            });
        }

        Ok(UpFile {
            common: CommonSpec {
                context: self.context.scope.borrow().context.clone(),
                ..CommonSpec::default()
            },
            services,
        })
    }

    fn run(&mut self) {
        let plan = match self.plan() {
            Ok(plan) => plan,
            Err(missing) => {
                self.problem = Some(format!("Needs {missing}."));
                return;
            }
        };
        let Some(config) = self.config() else {
            return;
        };
        match Launch::start(self.context.clone(), &plan, None) {
            Ok(launch) => {
                for service in &self.services {
                    let job = self.job_for(&config, service);
                    if let Some((command, _)) = self.command(service, job) {
                        history::record(
                            history::target_key(&self.target_spec(service, job)),
                            Some(service.dir_text()),
                            command,
                        );
                    }
                }
                self.problem = None;
                self.launch = Some(launch);
            }
            Err(error) => self.problem = Some(format!("Could not start mirrord: {error:#}")),
        }
    }

    /// Moves to `step` when what it needs is there: the later steps need a folder.
    fn go_to(&mut self, step: Step) {
        if step > Step::Folders && self.services.is_empty() {
            self.problem = Some("Pick a folder first.".to_owned());
            return;
        }
        self.problem = None;
        self.step = step;
    }

    /// `Enter`: on to the next step, or run from the last one. On the first step with nothing
    /// ticked, the folder under the cursor is taken.
    fn next(&mut self) {
        if self.step == Step::Folders && self.services.is_empty() {
            match self.folders.list.focused().cloned() {
                Some(choice) if choice.pickable => self.toggle_folder(&choice.id),
                Some(choice) => {
                    self.folders.open(&choice.id);
                    return;
                }
                None => return,
            }
        }
        match self.step.next() {
            Some(next) => self.go_to(next),
            None => self.run(),
        }
    }

    fn back(&mut self) {
        if let Some(previous) = self.step.previous() {
            self.go_to(previous);
        }
    }

    fn is_picked_folder(&self, id: &str) -> bool {
        self.services
            .iter()
            .any(|service| service.dir.as_path() == Path::new(id))
    }

    /// Acts on what the list of the current step asked for. Returns whether it was handled.
    fn on_list(&mut self, event: ListEvent) -> bool {
        match (self.step, event) {
            (_, ListEvent::Nothing) => return false,
            (_, ListEvent::Moved) => {}
            (_, ListEvent::Next) => self.next(),
            (Step::Folders, ListEvent::Toggle(id)) => self.toggle_folder(&id),
            (Step::Folders, ListEvent::Open(id)) => self.folders.open(&id),
            (Step::Folders, ListEvent::Up) => {
                self.folders.up();
            }
            (Step::Folders, ListEvent::Back) => {
                if self.folders.place == folders::Place::Projects {
                    return false;
                }
                self.folders.back_to_projects();
            }
            (Step::Folders, ListEvent::ToggleAll) => {
                let shown = self.folders.list.shown_ids();
                let all = shown.iter().all(|id| self.is_picked_folder(id));
                for id in shown {
                    if self.is_picked_folder(&id) == all {
                        self.toggle_folder(&id);
                    }
                }
            }
            (Step::Targets, ListEvent::Toggle(id)) => self.toggle_target(&id),
            (Step::Targets, ListEvent::Open(id)) => {
                if !self
                    .picked
                    .iter()
                    .any(|picked| picked.candidate.label() == id)
                {
                    self.toggle_target(&id);
                }
            }
            (Step::Config, ListEvent::Toggle(id) | ListEvent::Open(id)) => {
                self.config_choice = Some(id);
                self.problem = None;
            }
            (_, ListEvent::Back | ListEvent::Up) => self.back(),
            (_, ListEvent::ToggleAll) => {}
        }
        true
    }

    fn list_mut(&mut self) -> &mut SelectList {
        match self.step {
            Step::Folders => &mut self.folders.list,
            Step::Targets => &mut self.targets,
            Step::Config => &mut self.configs,
        }
    }

    /// `Ctrl+E`: change a folder's command, asking which folder first when there are several.
    fn edit_command(&mut self) {
        if let [service] = self.services.as_slice() {
            let dir = service.dir.clone();
            self.open_command_picker(dir);
            return;
        }
        let names = self.services.iter().map(Service::name).collect();
        self.picker = Some((Pick::Folder, Picker::new("Command for", names, None)));
    }

    fn open_command_picker(&mut self, dir: PathBuf) {
        let Some(service) = self.service(&dir) else {
            return;
        };
        let job = self
            .config()
            .map_or(Job::NoTraffic, |config| self.job_for(&config, service));
        let mut items: Vec<String> = Vec::new();
        if let Some(past) = history::recall(&history::target_key(&self.target_spec(service, job))) {
            items.extend(past.commands);
        }
        for command in &service.detected_commands {
            if !items.contains(command) {
                items.push(command.clone());
            }
        }
        items.push(TYPE_OWN.to_owned());
        let current = self.command(service, job).map(|(command, _)| command);
        self.picker = Some((Pick::Command(dir), Picker::new("Command", items, current)));
    }

    fn picked_from_picker(&mut self, pick: Pick, choice: String) {
        match pick {
            Pick::Folder => {
                if let Some(dir) = self
                    .services
                    .iter()
                    .find(|service| service.name() == choice)
                    .map(|service| service.dir.clone())
                {
                    self.open_command_picker(dir);
                }
            }
            Pick::Command(dir) if choice == TYPE_OWN => {
                let mut input = TextInput::default();
                if let Some(service) = self.service(&dir)
                    && let Some((command, _)) = self.command(service, Job::NoTraffic)
                {
                    input.paste(&command);
                    input.select_all();
                }
                self.editor = Some((dir, input));
            }
            Pick::Command(dir) => {
                if let Some(service) = self.services.iter_mut().find(|service| service.dir == dir) {
                    service.command_choice = Some(choice);
                }
            }
        }
    }

    /// Whether the TUI was started inside a project. The application opens on this view then,
    /// since starting a session is what someone in a project folder most likely came to do.
    pub fn in_project(&self) -> bool {
        self.services.first().is_some_and(Service::is_project)
    }

    fn target_choices(&self) -> Vec<Choice> {
        // Each target once, with the folder it fits best.
        let mut best: BTreeMap<String, (u32, usize, Candidate)> = BTreeMap::new();
        for (index, service) in self.services.iter().enumerate() {
            for candidate in &service.candidates {
                let label = candidate.label();
                let better = best
                    .get(&label)
                    .is_none_or(|(score, _, _)| candidate.score > *score);
                if better {
                    best.insert(label, (candidate.score, index, candidate.clone()));
                }
            }
        }
        let mut suggested: Vec<(u32, usize, Candidate)> = Vec::new();
        let mut rest: Vec<Candidate> = Vec::new();
        for (score, index, candidate) in best.into_values() {
            // Pods and replica sets belong to a workload that is the better pick, so they are not
            // suggested even when their names match.
            let instance = matches!(candidate.kind, TargetKind::Pod | TargetKind::ReplicaSet);
            if instance || candidate.reason.starts_with("no name match") {
                rest.push(candidate);
            } else {
                suggested.push((score, index, candidate));
            }
        }
        suggested.sort_by_key(|(score, _, _)| Reverse(*score));
        rest.sort_by(|left, right| {
            (&left.namespace, left.path()).cmp(&(&right.namespace, right.path()))
        });

        let several = self.services.len() > 1;
        let namespace = |candidate: &Candidate| {
            vec![Span::styled(
                candidate.namespace.clone(),
                Style::default().fg(theme::SKY),
            )]
        };
        let mut choices: Vec<Choice> = suggested
            .into_iter()
            .map(|(_, index, candidate)| {
                let folder = self
                    .services
                    .get(index)
                    .map(Service::name)
                    .unwrap_or_default();
                Choice {
                    id: candidate.label(),
                    section: Some("suggested"),
                    mark: Some(if candidate.is_sure() {
                        Span::styled("\u{2605}", Style::default().fg(theme::MINT))
                    } else {
                        Span::styled("\u{2606}", Style::default().fg(theme::AMBER))
                    }),
                    title: candidate.path(),
                    tone: Some(theme::TEAL),
                    // The reason without where the name came from; the details pane has it whole.
                    detail: {
                        let reason = candidate
                            .reason
                            .split(" (")
                            .next()
                            .unwrap_or_default()
                            .to_owned();
                        if several {
                            format!("for {folder} \u{b7} {reason}")
                        } else {
                            reason
                        }
                    },
                    note: namespace(&candidate),
                    keywords: format!("{} {} {folder}", candidate.namespace, candidate.kind),
                    pickable: true,
                }
            })
            .collect();
        // Pods and replica sets come last: each belongs to a workload listed above it.
        rest.sort_by_key(|candidate| {
            matches!(candidate.kind, TargetKind::Pod | TargetKind::ReplicaSet)
        });
        choices.extend(rest.into_iter().map(|candidate| Choice {
            id: candidate.label(),
            section: Some(
                if matches!(candidate.kind, TargetKind::Pod | TargetKind::ReplicaSet) {
                    "pods and replica sets"
                } else {
                    "all targets"
                },
            ),
            mark: Some(Span::raw(" ")),
            title: candidate.path(),
            tone: Some(theme::TEAL),
            note: namespace(&candidate),
            keywords: candidate.namespace.clone(),
            pickable: true,
            ..Choice::default()
        }));
        choices
    }

    fn config_choices(options: &[ConfigOption]) -> Vec<Choice> {
        options
            .iter()
            .enumerate()
            .map(|(index, option)| {
                let made = matches!(option.setup, Setup::Made { .. });
                let tone = match &option.setup {
                    Setup::Made { job, .. } => job.tone(),
                    Setup::File { .. } => theme::LAVENDER,
                };
                let note = if index == 0 && made && option.id != "made:none" {
                    vec![Span::styled(" recommended ", theme::chip(theme::MINT))]
                } else {
                    Vec::new()
                };
                Choice {
                    id: option.id.clone(),
                    section: Some(if made {
                        "made from your picks"
                    } else {
                        "files in your folders"
                    }),
                    mark: Some(if made {
                        Span::styled("\u{2726}", Style::default().fg(theme::INDIGO))
                    } else {
                        Span::styled("\u{2261}", Style::default().fg(theme::INDIGO_LIGHT))
                    }),
                    title: option.title.clone(),
                    tone: Some(tone),
                    detail: option.detail.clone(),
                    note,
                    keywords: option.keywords.clone(),
                    pickable: true,
                }
            })
            .collect()
    }

    /// The step bar: each step with its number, done steps in mint with what was picked, the
    /// current one on an indigo chip. Clicking a step goes there.
    fn draw_step_bar(&mut self, frame: &mut Frame, area: Rect) {
        self.step_hits.clear();
        let mut spans = Vec::new();
        let mut x = area.x;
        for (index, step) in Step::ALL.iter().enumerate() {
            let count = match step {
                Step::Folders => self.services.len(),
                Step::Targets => self.picked.len(),
                Step::Config => 0,
            };
            let order = step.cmp(&self.step);
            let text = if order == Ordering::Less && count > 0 {
                format!(" {} {} \u{2713} {count} ", index + 1, step.name())
            } else {
                format!(" {} {} ", index + 1, step.name())
            };
            let style = match order {
                Ordering::Equal => theme::chip(theme::INDIGO),
                Ordering::Less => Style::default()
                    .fg(theme::MINT)
                    .add_modifier(Modifier::BOLD),
                Ordering::Greater => theme::muted(),
            };
            let width = u16::try_from(text.chars().count()).unwrap_or(u16::MAX);
            self.step_hits.push((Rect::new(x, area.y, width, 1), *step));
            spans.push(Span::styled(text, style));
            spans.push(Span::styled(" \u{203a} ", theme::muted()));
            x += width + 3;
        }
        spans.push(Span::styled(
            "\u{25b6} run",
            if self.step == Step::Config && self.plan().is_ok() {
                Style::default()
                    .fg(theme::MINT)
                    .add_modifier(Modifier::BOLD)
            } else {
                theme::muted()
            },
        ));
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn draw_wizard(&mut self, frame: &mut Frame, area: Rect) {
        let namespace = self
            .discovery
            .read()
            .map(|discovery| discovery.namespace.clone())
            .unwrap_or_default();
        let note = (!namespace.is_empty()).then(|| format!("in {namespace}"));
        let block = panel::block("run", None, note.as_deref(), None);
        let inner = block.inner(area).inner(Margin::new(1, 0));
        frame.render_widget(block, area);

        let [
            bar_area,
            _,
            question_area,
            _,
            body_area,
            _,
            tray_area,
            legend_area,
        ] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(inner);

        self.draw_step_bar(frame, bar_area);
        frame.render_widget(
            Paragraph::new(Line::from_iter([
                Span::styled(
                    self.step.question(),
                    Style::default()
                        .fg(theme::LAVENDER)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("  {}", self.step.help()), theme::muted()),
            ])),
            question_area,
        );

        let (list_area, details_area) = if body_area.width >= WIDE_LAYOUT {
            let [list, details] =
                Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)])
                    .spacing(2)
                    .areas(body_area);
            (list, Some(details))
        } else if body_area.height >= TALL_LAYOUT {
            let [list, details] =
                Layout::vertical([Constraint::Percentage(55), Constraint::Percentage(45)])
                    .areas(body_area);
            (list, Some(details))
        } else {
            (body_area, None)
        };

        match self.step {
            Step::Folders => {
                let [location_area, list_area] =
                    Layout::vertical([Constraint::Length(1), Constraint::Fill(1)])
                        .spacing(1)
                        .areas(list_area);
                frame.render_widget(Paragraph::new(self.folders.location()), location_area);
                let picked: BTreeSet<String> =
                    self.services.iter().map(Service::dir_text).collect();
                self.folders
                    .list
                    .draw(frame, list_area, &|id| picked.contains(id));
                if let Some(details_area) = details_area {
                    details_pane(frame, details_area, "folder", self.folders.details());
                }
                let chips: Vec<(String, String)> = self
                    .services
                    .iter()
                    .map(|service| (service.dir_text(), service.name()))
                    .collect();
                self.tray.draw(
                    frame,
                    tray_area,
                    "picked",
                    &chips,
                    "nothing yet - space picks the folder under the cursor",
                );
            }
            Step::Targets => {
                let choices = self.target_choices();
                let empty = choices.is_empty();
                self.targets.set_choices(choices);
                let picked: BTreeSet<String> = self
                    .picked
                    .iter()
                    .map(|picked| picked.candidate.label())
                    .collect();
                if empty {
                    frame.render_widget(
                        Paragraph::new(self.targets_waiting()).wrap(Wrap { trim: false }),
                        list_area,
                    );
                } else {
                    self.targets
                        .draw(frame, list_area, &|id| picked.contains(id));
                }
                if let Some(details_area) = details_area {
                    let lines = self.target_details();
                    details_pane(frame, details_area, "target", lines);
                }
                let chips: Vec<(String, String)> = self
                    .picked
                    .iter()
                    .map(|picked| {
                        let folder = self
                            .service(&picked.dir)
                            .map(Service::name)
                            .unwrap_or_default();
                        (
                            picked.candidate.label(),
                            format!("{folder} \u{2192} {}", picked.candidate.path()),
                        )
                    })
                    .collect();
                self.tray.draw(
                    frame,
                    tray_area,
                    "picked",
                    &chips,
                    "none - folders without a target run with no incoming traffic",
                );
            }
            Step::Config => {
                let options = self.config_options();
                self.configs.set_choices(Self::config_choices(&options));
                let chosen = self.config().map(|config| config.id).unwrap_or_default();
                self.configs.draw(frame, list_area, &|id| id == chosen);
                if let Some(details_area) = details_area {
                    let lines = self.plan_lines();
                    details_pane(
                        frame,
                        details_area,
                        if self.show_yaml {
                            "yaml"
                        } else {
                            "what will run"
                        },
                        lines,
                    );
                }
                frame.render_widget(Paragraph::new(self.status_line()), tray_area);
            }
        }

        let legend = match self.step {
            Step::Folders => vec![
                Hint::new("space", "pick"),
                Hint::new("\u{2192}", "open folder"),
                Hint::new("\u{2190}", "up"),
                Hint::new("enter", "next"),
            ],
            Step::Targets => {
                let mut legend = vec![Hint::new("space", "pick")];
                if self.services.len() > 1 {
                    legend.push(Hint::new("ctrl+f", "other folder"));
                }
                legend.extend([Hint::new("enter", "next"), Hint::new("esc", "back")]);
                legend
            }
            Step::Config => vec![
                Hint::new("space", "choose"),
                Hint::new("enter", "run"),
                Hint::new("ctrl+e", "command"),
                Hint::new("ctrl+y", if self.show_yaml { "summary" } else { "yaml" }),
                Hint::new("esc", "back"),
            ],
        };
        frame.render_widget(Paragraph::new(hints::legend(&legend)), legend_area);

        if let Some(problem) = &self.problem
            && self.step != Step::Config
        {
            frame.render_widget(
                Paragraph::new(Line::from_iter([
                    Span::styled(" ! ", theme::chip(theme::AMBER)),
                    Span::styled(format!(" {problem}"), theme::warning()),
                ])),
                tray_area,
            );
        }
    }

    /// What the targets list says while there is nothing to list.
    fn targets_waiting(&self) -> Vec<Line<'static>> {
        let state = self
            .discovery
            .read()
            .ok()
            .and_then(|discovery| match &discovery.targets {
                Fetch::Ready(_) => None,
                Fetch::Failed(error) => Some(Line::styled(error.clone(), theme::error())),
                Fetch::Disconnected => Some(Line::styled(
                    "Waiting for the cluster... Enter goes on without a target.",
                    theme::muted(),
                )),
                Fetch::Loading => Some(Line::styled("Looking for targets...", theme::muted())),
            });
        vec![state.unwrap_or_else(|| {
            Line::styled(
                "Nothing to target here. Try :ns, or Enter to go on without a target.",
                theme::warning(),
            )
        })]
    }

    fn target_details(&self) -> Vec<Line<'static>> {
        let Some(focused) = self.targets.focused() else {
            return Vec::new();
        };
        let Some(candidate) = self
            .services
            .iter()
            .flat_map(|service| service.candidates.iter())
            .find(|candidate| candidate.label() == focused.id)
            .cloned()
        else {
            return Vec::new();
        };
        let row = |label: &'static str, value: String, color: Color| {
            Line::from_iter([
                Span::styled(format!("{label:<10}"), theme::muted()),
                Span::styled(value, Style::default().fg(color)),
            ])
        };
        let mut lines = vec![
            Line::styled(candidate.path(), theme::title().fg(theme::TEAL)),
            Line::raw(""),
            row("namespace", candidate.namespace.clone(), theme::SKY),
            row("kind", candidate.kind.to_string(), theme::LAVENDER),
            row(
                "why",
                candidate.reason.clone(),
                if candidate.is_sure() {
                    theme::MINT
                } else {
                    theme::AMBER
                },
            ),
        ];
        if let Some(picked) = self
            .picked
            .iter()
            .find(|picked| picked.candidate.label() == candidate.label())
        {
            lines.push(row(
                "runs for",
                self.service(&picked.dir)
                    .map(Service::name)
                    .unwrap_or_default(),
                theme::MINT,
            ));
        }
        lines.push(Line::raw(""));
        let inspected =
            self.inspection
                .read()
                .ok()
                .and_then(|found| match found.get(&candidate.label()) {
                    Some(Fetch::Ready(resources)) => Some(Some(resources.clone())),
                    Some(_) => Some(None),
                    None => None,
                });
        match inspected {
            Some(Some(resources)) => {
                for database in &resources.databases {
                    lines.push(row(
                        "database",
                        format!("{} \u{b7} {}", database.engine.name(), database.label()),
                        theme::LAVENDER,
                    ));
                    lines.push(Line::styled(
                        format!("          {}", database.credentials()),
                        theme::muted(),
                    ));
                }
                for queue in &resources.queues {
                    lines.push(row(
                        "queue",
                        format!("{} \u{b7} {}", queue.id, queue.kind),
                        theme::LAVENDER,
                    ));
                }
                if resources.databases.is_empty() && resources.queues.is_empty() {
                    lines.push(Line::styled("no databases or queues found", theme::muted()));
                }
            }
            Some(None) => lines.push(Line::styled("reading the pod spec...", theme::muted())),
            None => {}
        }
        lines
    }

    /// The config step's preview: each folder with its target, command and traffic, or the YAML
    /// mirrord reads.
    fn plan_lines(&self) -> Vec<Line<'static>> {
        let plan = match self.plan() {
            Ok(plan) => plan,
            Err(missing) => {
                return vec![Line::styled(format!("Needs {missing}."), theme::warning())];
            }
        };
        if self.show_yaml {
            return plan
                .to_yaml()
                .unwrap_or_default()
                .lines()
                .map(|line| Line::styled(line.to_owned(), theme::muted()))
                .collect();
        }
        let Some(config) = self.config() else {
            return Vec::new();
        };
        let row = |label: &'static str, value: String, color: Color| {
            Line::from_iter([
                Span::styled(format!("  {label:<8}"), theme::muted()),
                Span::styled(value, Style::default().fg(color)),
            ])
        };
        let mut lines = Vec::new();
        for service in &self.services {
            let job = self.job_for(&config, service);
            lines.push(Line::from_iter([
                Span::styled(service.name(), theme::title()),
                Span::styled(
                    format!(
                        "  {}",
                        ellipsize(
                            &suggest::compress_home(&service.dir_text()),
                            MAX_PREVIEW_PATH
                        )
                    ),
                    theme::muted(),
                ),
            ]));
            match self.target_spec(service, job) {
                TargetSpec::Path { path, namespace } => {
                    lines.push(row("target", path, theme::TEAL));
                    lines.push(row("in", namespace.unwrap_or_default(), theme::SKY));
                }
                TargetSpec::Targetless => lines.push(row("target", "none".to_owned(), theme::GREY)),
            }
            lines.push(row("traffic", job.traffic().to_owned(), job.tone()));
            if let Some((command, source)) = self.command(service, job) {
                let tone = if source == RAN_LAST_TIME || service.command_choice.is_some() {
                    theme::MINT
                } else {
                    theme::AMBER
                };
                lines.push(row("runs", format!("$ {command}"), theme::LAVENDER));
                lines.push(Line::styled(
                    format!("          {source}"),
                    Style::default().fg(tone),
                ));
            }
            if let Some(patch) = self.config_patch(&config, service) {
                let branches = patch
                    .pointer("/feature/db_branches")
                    .and_then(Value::as_array)
                    .map(Vec::len);
                lines.push(match branches {
                    Some(count) => row("db", format!("{count} branched"), theme::MINT),
                    None => row("config", "from the file".to_owned(), theme::LAVENDER),
                });
            }
            lines.push(Line::raw(""));
        }
        lines
    }

    fn status_line(&self) -> Line<'static> {
        match (&self.problem, self.plan()) {
            (Some(problem), _) => Line::from_iter([
                Span::styled(" FAILED ", theme::chip(theme::CORAL)),
                Span::styled(format!(" {problem}"), theme::error()),
            ]),
            (None, Ok(plan)) => Line::from_iter([
                Span::styled(" \u{25cf} READY ", theme::chip(theme::MINT)),
                Span::styled("  enter ", theme::title()),
                Span::styled(
                    match plan.services.len() {
                        1 => "starts it".to_owned(),
                        count => format!("starts all {count}"),
                    },
                    Style::default().fg(theme::LAVENDER),
                ),
            ]),
            (None, Err(missing)) => Line::from_iter([
                Span::styled(" NEEDS ", theme::chip(theme::AMBER)),
                Span::styled(format!(" {missing}"), theme::warning()),
            ]),
        }
    }

    fn handle_key(&mut self, key: &KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let searching = !self.list_mut().search().is_empty();
        match (self.step, key.code) {
            (Step::Targets, KeyCode::Char('f')) if ctrl => self.cycle_target_folder(),
            (Step::Config, KeyCode::Char('e')) if ctrl => self.edit_command(),
            (Step::Config, KeyCode::Char('y')) if ctrl => self.show_yaml = !self.show_yaml,
            // A path's first character jumps there, the way a shell reads it.
            (Step::Folders, KeyCode::Char('~')) if !searching => {
                if let Some(home) = folders::home() {
                    self.folders.go_to(home, None);
                }
            }
            (Step::Folders, KeyCode::Char('/')) if !searching => {
                self.folders.go_to(PathBuf::from("/"), None);
            }
            _ => {
                let event = self.list_mut().handle_key(key);
                return self.on_list(event);
            }
        }
        true
    }
}

/// A bordered pane next to the list, describing what the cursor is on.
fn details_pane(frame: &mut Frame, area: Rect, title: &str, lines: Vec<Line<'static>>) {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::BORDER_DIM))
        .padding(Padding::horizontal(1))
        .title(Span::styled(format!(" {title} "), theme::table_header()));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// Namespaces that never hold the user's services.
const SYSTEM_NAMESPACES: &[&str] = &["kube-system", "kube-public", "kube-node-lease", "mirrord"];

/// Most namespaces searched when none is picked. Each one costs a request per target kind, so a
/// cluster with more than this is searched in its default namespace only, and `:ns` picks
/// another.
const MAX_SEARCHED_NAMESPACES: usize = 30;

/// The namespaces to look for targets in when the user has not picked one: every namespace but
/// the system ones, or only the client's default namespace when there are too many or they
/// cannot be listed.
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

impl Screen for RunScreen {
    fn new(context: Context) -> Self {
        // The folder the TUI started in is picked when it is a project, and the view starts on
        // the targets: the folder question is already answered.
        let services: Vec<Service> = std::env::current_dir()
            .map(Service::new)
            .into_iter()
            .filter(Service::is_project)
            .collect();
        let step = if services.is_empty() {
            Step::Folders
        } else {
            Step::Targets
        };

        let discovery = Arc::new(RwLock::new(Discovery::default()));
        let inspection = Arc::new(RwLock::new(Inspections::default()));
        let found = Arc::new(RwLock::new(None));
        let (inspect, inspect_rx) = watch::channel(Vec::new());
        tokio::spawn(Self::inspect_targets(
            context.clone(),
            inspect_rx,
            inspection.clone(),
        ));
        tokio::spawn(Self::discover(context.clone(), discovery.clone()));

        // The projects for the folder step, looked for once in the background.
        if let Some(home) = folders::home() {
            let found = found.clone();
            let context = context.clone();
            tokio::task::spawn_blocking(move || {
                let projects = projects::scan(&home);
                if let Ok(mut slot) = found.write() {
                    *slot = Some(projects);
                }
                context.request_redraw();
            });
        }

        let recent = history::dirs()
            .into_iter()
            .map(PathBuf::from)
            .filter(|dir| dir.is_dir())
            .collect();

        Self {
            context,
            step,
            services,
            folders: FolderStep::new(
                recent,
                SelectList::new(
                    Picks::Many,
                    "type to search folders, ~ for home, / for root",
                ),
            ),
            targets: SelectList::new(Picks::Many, "type to search targets"),
            picked: Vec::new(),
            auto_picked: BTreeSet::new(),
            configs: SelectList::new(Picks::One, "type to search: db, kafka, steal, a file..."),
            config_choice: None,
            discovery,
            found,
            inspection,
            inspect,
            tray: Tray::default(),
            step_hits: Vec::new(),
            show_yaml: false,
            picker: None,
            editor: None,
            launch: None,
            problem: None,
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        if let Some(launch) = &mut self.launch {
            launch.draw(frame, area, true);
            return;
        }

        self.rank_services();
        self.auto_pick_targets();
        self.folders.set_picked(
            self.services
                .iter()
                .map(|service| service.dir.clone())
                .collect(),
        );
        if self.folders.is_searching()
            && let Some(found) = self.found.read().ok().and_then(|found| found.clone())
        {
            self.folders.set_found(found);
        }

        // Inspect what is picked, and what the cursor is on in the targets step.
        let mut wanted: Vec<Candidate> = self
            .picked
            .iter()
            .map(|picked| picked.candidate.clone())
            .collect();
        if self.step == Step::Targets
            && let Some(focused) = self.targets.focused()
            && let Some(candidate) = self
                .services
                .iter()
                .flat_map(|service| service.candidates.iter())
                .find(|candidate| candidate.label() == focused.id)
        {
            wanted.push(candidate.clone());
        }
        self.inspect.send_if_modified(|current| {
            let labels = |list: &[Candidate]| list.iter().map(Candidate::label).collect::<Vec<_>>();
            let differs = labels(current) != labels(&wanted);
            if differs {
                *current = wanted;
            }
            differs
        });

        self.draw_wizard(frame, area);

        if let Some((_, picker)) = &self.picker {
            picker.draw(frame, area);
        }
        if let Some((dir, input)) = &self.editor {
            let name = self.service(dir).map(Service::name).unwrap_or_default();
            let inner = dialog(
                frame,
                centered(area, Constraint::Length(72), Constraint::Length(5)),
                &format!("Command for {name}"),
            );
            let mut spans = vec![Span::styled("$ ", theme::title())];
            spans.extend(input.spans(
                Style::default().fg(theme::LAVENDER),
                Style::default().fg(theme::INDIGO),
                true,
            ));
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from(spans),
                    Line::raw(""),
                    hints::legend(&[Hint::new("enter", "keep"), Hint::new("esc", "cancel")]),
                ]),
                inner,
            );
        }
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

        if let Some((dir, input)) = &mut self.editor {
            match &event {
                Event::Paste(text) => input.paste(text),
                Event::Key(key) => match key.code {
                    KeyCode::Enter => {
                        let command = input.value().trim().to_owned();
                        let dir = dir.clone();
                        self.editor = None;
                        if let Some(service) =
                            self.services.iter_mut().find(|service| service.dir == dir)
                        {
                            service.command_choice = (!command.is_empty()).then_some(command);
                        }
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

        if let Some((_, picker)) = &mut self.picker {
            if let Event::Key(key) = event {
                match picker.handle_key(key) {
                    PickerOutcome::Open => {}
                    PickerOutcome::Cancelled => self.picker = None,
                    PickerOutcome::Picked(choice) => {
                        if let Some((pick, _)) = self.picker.take() {
                            self.picked_from_picker(pick, choice);
                        }
                    }
                }
            }
            return ControlFlow::Break(());
        }

        let handled = match &event {
            Event::Key(key) => self.handle_key(key),
            Event::Paste(text) => {
                self.list_mut().paste(text);
                true
            }
            Event::Mouse(mouse) => {
                let at = Position::new(mouse.column, mouse.row);
                let step = self
                    .step_hits
                    .iter()
                    .find(|(area, _)| area.contains(at))
                    .map(|(_, step)| *step)
                    .filter(|_| mouse.kind == MouseEventKind::Down(MouseButton::Left));
                if let Some(step) = step {
                    self.go_to(step);
                    true
                } else if let Some(id) = self.tray.clicked(mouse) {
                    match self.step {
                        Step::Folders => self.toggle_folder(&id),
                        Step::Targets => self.toggle_target(&id),
                        Step::Config => {}
                    }
                    true
                } else {
                    let event = self.list_mut().handle_mouse(mouse);
                    self.on_list(event)
                }
            }
            _ => false,
        };
        if handled {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(event)
        }
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
        let mut hints = vec![
            Hint::new("space/click", "pick"),
            Hint::new(
                "enter",
                if self.step == Step::Config {
                    "run"
                } else {
                    "next"
                },
            ),
        ];
        if self.step.previous().is_some() {
            hints.push(Hint::new("esc", "back"));
        }
        hints.push(Hint::new("type", "search"));
        hints
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_go_forward_and_back_in_order() {
        assert_eq!(Step::Folders.next(), Some(Step::Targets));
        assert_eq!(Step::Config.next(), None);
        assert_eq!(Step::Config.previous(), Some(Step::Targets));
        assert_eq!(Step::Folders.previous(), None);
    }
}
