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
    mem,
    ops::ControlFlow,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
use futures_util::future::join_all;
use k8s_openapi::{api::core::v1::Namespace, jiff::Timestamp};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Margin, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Padding, Paragraph, Wrap},
};
use serde_json::{Map, Value, json};
use tokio::sync::watch;

use crate::{
    context::Context,
    helpers::{centered, dialog, ellipsize},
    kit::{
        hints::{self, Hint},
        panel,
        select_list::{Choice, ListEvent, Picks, SelectList},
        state_view::Fetch,
        text_input::{FieldLook, TextInput},
        tray::Tray,
    },
    screens::{
        Screen,
        config::ConfigScreen,
        targets::{
            browser::{self, TargetItem, TargetKind},
            form::split_command,
            history::{self, RecipeService, RunRecipe},
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
            Self::Config => "Every option of this run, as on the Config screen.",
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
    /// Which folder a new config is saved in.
    SaveFolder,
    /// The command for the folder at this path.
    Command(PathBuf),
}

/// The id prefix of the folder step's "run again" rows, followed by the run's index.
const RUN_AGAIN: &str = "run-again:";

/// The start-from dialog's row that asks for a config file's path.
const LOAD_A_FILE: &str = "action:load-a-file";

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
    /// The name being typed for a new config, and the folder it goes in.
    naming: Option<(PathBuf, TextInput)>,
    /// The run's options: every mirrord option, shown and edited the way the Config screen does,
    /// on top of what "start from" picked. A new config until saved (`Ctrl+S`), or a saved one
    /// loaded from a folder.
    working: Option<ConfigScreen>,
    /// The path being typed to load a config file from anywhere.
    loading: Option<TextInput>,
    /// The "start from" dialog (`Ctrl+O`): the made configs, the saved ones, and loading a file
    /// by its path, searchable by what they do.
    start_list: Option<SelectList>,
    launch: Option<Launch>,
    /// Whether the run's log fills the view. `Esc` hides it while the run goes on, so the steps
    /// stay reachable; `Ctrl+L` or a click on the step bar's `● running` brings it back.
    log_open: bool,
    /// Where the step bar's run chip landed, for clicks.
    run_hit: Option<Rect>,
    /// Targets of a run being set up again, by folder and [`Candidate::label`], picked as soon as
    /// the cluster lists them.
    pending_targets: Vec<(PathBuf, String)>,
    /// Started inside a project: once every folder has a sure target, go straight to the config
    /// step. Off as soon as the user presses a key here, so it never takes the wheel back.
    auto_advance: bool,
    /// Whether settings were already filled in for the current options (the folder's own config,
    /// the port mapping), so they are filled in once and never fought over.
    filled: bool,
    /// What was just done for the user (`picked for you: ...`), shown until their next key.
    notice: Option<String>,
    /// Whether the folder step's "run again" rows are current; cleared by every new run.
    ran_again_rows_set: bool,
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

    /// The config patch a folder runs with: what "start from" adds for it (its branched
    /// databases), with the options edited on the config step over it.
    fn config_patch(&self, config: &ConfigOption, service: &Service) -> Option<Value> {
        let edits = self
            .working
            .as_ref()
            .map(|working| working.options().clone())
            .filter(|options| !options.is_empty())
            .map(Value::Object);
        match (self.preset_patch(config, service), edits) {
            (Some(mut preset), Some(edits)) => {
                merge(&mut preset, edits);
                Some(preset)
            }
            (preset, edits) => preset.or(edits),
        }
    }

    /// What a made config adds for one folder: its target's databases, branched.
    fn preset_patch(&self, config: &ConfigOption, service: &Service) -> Option<Value> {
        match &config.setup {
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
        // One run at a time: a second Enter shows the one going on.
        if self.launch.as_ref().is_some_and(Launch::running) {
            self.log_open = true;
            return;
        }
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
                self.remember_run(&config);
                self.problem = None;
                self.launch = Some(launch);
                self.log_open = true;
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
            (Step::Folders, ListEvent::Open(id)) if id.starts_with(RUN_AGAIN) => {
                self.run_again(&id);
            }
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
            (Step::Config, _) => {}
            (_, ListEvent::Back | ListEvent::Up) => self.back(),
            (_, ListEvent::ToggleAll) => {}
        }
        true
    }

    /// The list of the current step; the config step has the option tree instead.
    fn list_mut(&mut self) -> Option<&mut SelectList> {
        match self.step {
            Step::Folders => Some(&mut self.folders.list),
            Step::Targets => Some(&mut self.targets),
            Step::Config => None,
        }
    }

    /// Adds to what is shown as done for the user, after anything already said.
    fn tell(&mut self, text: String) {
        self.notice = Some(match self.notice.take() {
            Some(earlier) => format!("{earlier} \u{b7} {text}"),
            None => text,
        });
    }

    /// Remembers this run as a whole, for running it again from the folder step.
    fn remember_run(&mut self, config: &ConfigOption) {
        let services = self
            .services
            .iter()
            .map(|service| {
                let job = self.job_for(config, service);
                RecipeService {
                    dir: service.dir_text(),
                    target: self.target_of(&service.dir).map(Candidate::label),
                    command: self.command(service, job).map(|(command, _)| command),
                }
            })
            .collect();
        let options = self
            .working
            .as_ref()
            .filter(|working| working.is_scratch() && !working.options().is_empty())
            .map(|working| Value::Object(working.options().clone()));
        history::record_run(RunRecipe {
            cluster: String::new(),
            services,
            start_from: Some(config.id.clone()),
            options,
            ran_at: Timestamp::now().as_second(),
        });
        self.ran_again_rows_set = false;
    }

    /// Sets up an earlier run again, by its `run-again:<index>` row: its folders and commands at
    /// once, its targets as soon as the cluster lists them, its config, and the config step,
    /// ready to run.
    fn run_again(&mut self, id: &str) {
        let Some(recipe) = id
            .strip_prefix(RUN_AGAIN)
            .and_then(|index| index.parse::<usize>().ok())
            .and_then(|index| history::runs().into_iter().nth(index))
        else {
            return;
        };
        self.services.clear();
        self.picked.clear();
        self.pending_targets.clear();
        for saved in &recipe.services {
            let dir = PathBuf::from(&saved.dir);
            if !dir.is_dir() {
                continue;
            }
            let mut service = Service::new(dir.clone());
            service.command_choice = saved.command.clone();
            // Its target comes back below; no other guess should take its place.
            self.auto_picked.insert(dir.clone());
            if let Some(target) = &saved.target {
                self.pending_targets.push((dir, target.clone()));
            }
            self.services.push(service);
        }
        if self.services.is_empty() {
            self.problem = Some("None of that run's folders are there any more.".to_owned());
            return;
        }
        self.config_choice = recipe.start_from.clone();
        self.working = None;
        self.ensure_working();
        if let (Some(working), Some(Value::Object(options))) = (&mut self.working, recipe.options) {
            working.seed(options);
        }
        // What was filled in then is part of what comes back; nothing to fill in again.
        self.filled = true;
        self.step = Step::Config;
        self.problem = None;
        self.notice = Some("set up like before - ctrl+r runs it".to_owned());
    }

    /// Picks a run-again target once its folder's targets are listed and it is among them.
    fn pick_pending_targets(&mut self) {
        let mut still = Vec::new();
        for (dir, label) in mem::take(&mut self.pending_targets) {
            let found = self
                .service(&dir)
                .and_then(|service| {
                    service
                        .candidates
                        .iter()
                        .find(|candidate| candidate.label() == label)
                })
                .cloned();
            match found {
                Some(candidate) => self.picked.push(PickedTarget { candidate, dir }),
                None => still.push((dir, label)),
            }
        }
        self.pending_targets = still;
    }

    /// Started inside a project: once every folder has its sure target, the targets step has
    /// nothing to ask, so the view goes on to the config step and says what it picked.
    fn auto_advance_when_known(&mut self) {
        if !self.auto_advance || self.step != Step::Targets {
            return;
        }
        let listed = self
            .services
            .iter()
            .all(|service| self.auto_picked.contains(&service.dir));
        if !listed {
            return;
        }
        self.auto_advance = false;
        let picks: Vec<String> = self
            .picked
            .iter()
            .map(|picked| picked.candidate.path())
            .collect();
        // Nothing sure to pick: the user has a choice to make here.
        if picks.len() < self.services.len() {
            return;
        }
        self.step = Step::Config;
        self.notice = Some(format!(
            "picked for you: {} - esc to change",
            picks.join(", ")
        ));
    }

    /// Fills in, once per new config, what the folders and the cluster already say: the
    /// folder's own `.mirrord/mirrord.json` when it has one, else a port mapping when the app
    /// listens locally on a port the target does not.
    fn fill_in_settings(&mut self) {
        if self.filled {
            return;
        }
        let Some(first) = self.services.first() else {
            return;
        };
        let dir = first.dir.clone();
        let working_is_new = self
            .working
            .as_ref()
            .is_some_and(|working| working.is_scratch() && !working.is_dirty());
        if !working_is_new {
            self.filled = true;
            return;
        }

        // The team's own config for the folder wins over anything guessed.
        if self.config_choice.is_none() {
            let own = [
                "mirrord.json",
                "mirrord.yaml",
                "mirrord.yml",
                "mirrord.toml",
            ]
            .iter()
            .map(|name| dir.join(".mirrord").join(name))
            .find(|path| path.is_file());
            if let Some(path) = own {
                let shown = format!(
                    "{}/.mirrord/{}",
                    dir.file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    path.file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default()
                );
                self.filled = true;
                self.load_config(path);
                self.tell(format!("using {shown}, the folder's own config"));
                return;
            }
        }

        // The port mapping needs the target's ports, which come with its inspection; until then,
        // try again on a later frame.
        let Some(candidate) = self.target_of(&dir).cloned() else {
            self.filled = true;
            return;
        };
        let Some(resources) = self.resources(&candidate) else {
            return;
        };
        self.filled = true;
        let Some((local, source)) = detect::local_port(&dir) else {
            return;
        };
        let [remote] = resources.ports.as_slice() else {
            return;
        };
        if local == *remote {
            return;
        }
        if let Some(working) = &mut self.working {
            working.set(
                &["feature", "network", "incoming", "port_mapping"],
                json!([[local, remote]]),
            );
            self.tell(format!(
                "set for you: your app's port {local} (from {source}) maps to the target's {remote}"
            ));
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
            Pick::SaveFolder => {
                if let Some(dir) = self
                    .services
                    .iter()
                    .find(|service| service.name() == choice)
                    .map(|service| service.dir.clone())
                {
                    self.ask_config_name(dir);
                }
            }
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

    /// The config step's options, made the first time the step is shown: a new config in the
    /// first folder's `.mirrord`, or the saved config "start from" points at.
    fn ensure_working(&mut self) {
        if self.working.is_some() {
            return;
        }
        let saved = self.config().and_then(|config| match config.setup {
            Setup::File { path } => Some(path),
            Setup::Made { .. } => None,
        });
        self.working = match saved {
            Some(path) => Some(ConfigScreen::for_file(path, Map::new())),
            None => self.services.first().map(|service| {
                let name = free_config_name(&service.dir);
                ConfigScreen::scratch(service.dir.join(".mirrord").join(format!("{name}.json")))
            }),
        };
    }

    /// `Ctrl+O`: the "start from" dialog, on the config in effect.
    fn open_start_picker(&mut self) {
        let options = self.config_options();
        let mut list = SelectList::new(Picks::One, "type to search: db, kafka, steal, a file...");
        list.set_choices(start_choices(&options));
        if let Some(config) = self.config() {
            list.focus(&config.id);
        }
        self.start_list = Some(list);
    }

    /// Acts on a "start from" choice by its id. A made config changes how the folders run and
    /// keeps the edited options; a saved config is loaded into the option tree in their place,
    /// unless there are edits that would be lost.
    fn start_from(&mut self, id: &str) {
        self.start_list = None;
        if id == LOAD_A_FILE {
            self.loading = Some(TextInput::default());
            return;
        }
        let setup = self
            .config_options()
            .into_iter()
            .find(|option| option.id == id)
            .map(|option| option.setup);
        match setup {
            Some(Setup::File { path }) => self.load_config(path),
            Some(Setup::Made { .. }) => {
                self.config_choice = Some(id.to_owned());
                self.problem = None;
            }
            None => {}
        }
    }

    /// What the "start from" dialog asked for.
    fn on_start_list(&mut self, event: ListEvent) {
        match event {
            ListEvent::Toggle(id) | ListEvent::Open(id) => self.start_from(&id),
            ListEvent::Next => {
                let focused = self
                    .start_list
                    .as_ref()
                    .and_then(|list| list.focused())
                    .map(|choice| choice.id.clone());
                if let Some(id) = focused {
                    self.start_from(&id);
                }
            }
            ListEvent::Back => self.start_list = None,
            _ => {}
        }
    }

    /// Loads the config file at `path` into the option tree, unless there are edits it would
    /// replace.
    fn load_config(&mut self, path: PathBuf) {
        if self.working.as_ref().is_some_and(ConfigScreen::is_dirty) {
            self.problem = Some("Save your edits first (ctrl+s), or undo them.".to_owned());
            return;
        }
        if !path.is_file() {
            self.problem = Some(format!("No config file at {}.", path.display()));
            return;
        }
        self.config_choice = Some(format!("file:{}", path.display()));
        self.working = Some(ConfigScreen::for_file(path, Map::new()));
        self.problem = None;
    }

    /// `Ctrl+S`: saves the options. A new config asks for its folder (when there are several)
    /// and its name first; a loaded one is written back to its file.
    fn save_working(&mut self) {
        let Some(working) = &mut self.working else {
            return;
        };
        if !working.is_scratch() {
            if let Err(error) = working.save_if_edited() {
                self.problem = Some(format!("Could not save the config: {error}"));
            }
            return;
        }
        if let [service] = self.services.as_slice() {
            let dir = service.dir.clone();
            self.ask_config_name(dir);
            return;
        }
        let names = self.services.iter().map(Service::name).collect();
        self.picker = Some((
            Pick::SaveFolder,
            Picker::new("Save the config in which folder?", names, None),
        ));
    }

    /// Saves the new config as `dir/.mirrord/<name>` (`.json` added when it has no extension).
    fn save_working_as(&mut self, dir: PathBuf, name: &str) {
        let Some(file) = config_file_name(name) else {
            return;
        };
        let Some(working) = &mut self.working else {
            return;
        };
        match working.save_as(dir.join(".mirrord").join(file)) {
            Ok(()) => self.problem = None,
            Err(error) => self.problem = Some(format!("Could not save the config: {error}")),
        }
    }

    /// The config step's own keys first, then the option tree's. Returns whether the key was
    /// used, so the rest of the interface gets the ones that were not.
    fn config_key(&mut self, key: &KeyEvent) -> bool {
        self.ensure_working();
        let Some(working) = &mut self.working else {
            return false;
        };
        match working.handle_event(Event::Key(*key)) {
            ControlFlow::Break(()) => true,
            // An Esc the tree leaves alone means it has nothing open: back to the targets.
            ControlFlow::Continue(Event::Key(key)) if key.code == KeyCode::Esc => {
                self.back();
                true
            }
            ControlFlow::Continue(_) => false,
        }
    }

    /// Opens the name box for a new config in `dir`, with a name no file there has yet, selected
    /// so typing replaces it.
    fn ask_config_name(&mut self, dir: PathBuf) {
        let name = free_config_name(&dir);
        let mut input = TextInput::default();
        input.paste(&name);
        input.select_all();
        self.naming = Some((dir, input));
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
        // While a run goes on behind the steps, the chip says so and a click shows its log.
        let (chip, style) = match &self.launch {
            Some(launch) if launch.running() => (
                " \u{25cf} running \u{b7} ctrl+l log ".to_owned(),
                theme::chip(theme::MINT),
            ),
            Some(_) => (
                " \u{25a0} stopped \u{b7} ctrl+l log ".to_owned(),
                theme::chip(theme::GREY),
            ),
            None if self.step == Step::Config && self.plan().is_ok() => (
                "\u{25b6} run".to_owned(),
                Style::default()
                    .fg(theme::MINT)
                    .add_modifier(Modifier::BOLD),
            ),
            None => ("\u{25b6} run".to_owned(), theme::muted()),
        };
        let width = u16::try_from(chip.chars().count()).unwrap_or(0);
        self.run_hit = self
            .launch
            .is_some()
            .then(|| Rect::new(x, area.y, width, 1));
        spans.push(Span::styled(chip, style));
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

        // The notice takes the line under the question, two when it is long, so it is read whole.
        let notice_rows = match &self.notice {
            Some(notice) if notice.chars().count() + 2 > usize::from(inner.width) => 2,
            _ => 1,
        };
        let [
            bar_area,
            _,
            question_area,
            notice_area,
            body_area,
            _,
            tray_area,
            legend_area,
        ] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(notice_rows),
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
        // What was just done for the user, on its own line so it is read in full.
        if let Some(notice) = &self.notice {
            frame.render_widget(
                Paragraph::new(Line::styled(
                    format!("\u{2714} {notice}"),
                    Style::default().fg(theme::MINT),
                ))
                .wrap(Wrap { trim: true }),
                notice_area,
            );
        }

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
                let [start_area, editor_area] =
                    Layout::vertical([Constraint::Length(1), Constraint::Fill(1)])
                        .spacing(1)
                        .areas(body_area);
                frame.render_widget(Paragraph::new(self.start_line()), start_area);
                self.ensure_working();
                // Ctrl+Y swaps the option tree for exactly what mirrord will run.
                match &mut self.working {
                    Some(working) if !self.show_yaml => working.draw(frame, editor_area),
                    _ => {
                        let lines = self.plan_lines();
                        details_pane(frame, editor_area, "what mirrord runs", lines);
                    }
                }
                frame.render_widget(Paragraph::new(self.status_line()), tray_area);
            }
        }

        // While a search is typed, Space is part of it and Enter picks what it found.
        let searching = self
            .list_mut()
            .is_some_and(|list| !list.search().is_empty());
        let legend = match self.step {
            _ if searching => vec![
                Hint::new("enter", "pick it"),
                Hint::new("\u{2191}\u{2193}", "choose"),
                Hint::new("esc", "clear search"),
            ],
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
                Hint::new("ctrl+r", "run"),
                Hint::new("ctrl+o", "start from"),
                Hint::new("ctrl+s", "save"),
                Hint::new("enter", "edit"),
                Hint::new("/", "search"),
                Hint::new(
                    "ctrl+y",
                    if self.show_yaml {
                        "options"
                    } else {
                        "what runs"
                    },
                ),
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
            if let Some(count) = self
                .config_patch(&config, service)
                .as_ref()
                .and_then(|patch| patch.pointer("/feature/db_branches"))
                .and_then(Value::as_array)
                .map(Vec::len)
            {
                lines.push(row("db", format!("{count} branched"), theme::MINT));
            }
            let edited = self
                .working
                .as_ref()
                .map_or(0, |working| working.options().len());
            if edited > 0 {
                lines.push(row(
                    "options",
                    format!("{edited} set on the config step"),
                    theme::LAVENDER,
                ));
            }
            lines.push(Line::raw(""));
        }
        // Then exactly what `mirrord up` reads.
        lines.push(Line::styled(
            "the plan mirrord reads",
            theme::table_header(),
        ));
        lines.extend(
            plan.to_yaml()
                .unwrap_or_default()
                .lines()
                .map(|line| Line::styled(line.to_owned(), theme::muted())),
        );
        lines
    }

    /// The config step's first line: what it starts from, and how to change it.
    fn start_line(&self) -> Line<'static> {
        let Some(config) = self.config() else {
            return Line::default();
        };
        let (mark, tone) = match &config.setup {
            Setup::Made { job, .. } => ("\u{2726}", job.tone()),
            Setup::File { .. } => ("\u{2261}", theme::LAVENDER),
        };
        let mut spans = vec![
            Span::styled("Start from  ", theme::muted()),
            Span::styled(
                format!("{mark} {}", config.title),
                Style::default().fg(tone).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  ctrl+o change", theme::muted()),
        ];
        if let Setup::Made { .. } = config.setup {
            spans.push(Span::styled(
                format!("   {}", config.detail),
                theme::muted().italic(),
            ));
        }
        Line::from(spans)
    }

    fn status_line(&self) -> Line<'static> {
        match (&self.problem, self.plan()) {
            (Some(problem), _) => Line::from_iter([
                Span::styled(" FAILED ", theme::chip(theme::CORAL)),
                Span::styled(format!(" {problem}"), theme::error()),
            ]),
            (None, Ok(plan)) => Line::from_iter([
                Span::styled(" \u{25cf} READY ", theme::chip(theme::MINT)),
                Span::styled("  ctrl+r ", theme::title()),
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
        let searching = self
            .list_mut()
            .is_some_and(|list| !list.search().is_empty());
        match (self.step, key.code) {
            (Step::Targets, KeyCode::Char('f')) if ctrl => self.cycle_target_folder(),
            (Step::Config, KeyCode::Char('r')) if ctrl => self.run(),
            (Step::Config, KeyCode::Char('o')) if ctrl => self.open_start_picker(),
            (Step::Config, KeyCode::Char('s')) if ctrl => self.save_working(),
            (Step::Config, KeyCode::Char('e')) if ctrl => self.edit_command(),
            (Step::Config, KeyCode::Char('y')) if ctrl => self.show_yaml = !self.show_yaml,
            (Step::Config, _) => return self.config_key(key),
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
                let Some(list) = self.list_mut() else {
                    return false;
                };
                let event = list.handle_key(key);
                return self.on_list(event);
            }
        }
        true
    }
}

/// The "start from" dialog's rows: the made configs, the saved ones, and loading a file.
fn start_choices(options: &[ConfigOption]) -> Vec<Choice> {
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
                    "saved in your folders"
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
        .chain([Choice {
            id: LOAD_A_FILE.to_owned(),
            section: Some("load"),
            mark: Some(Span::styled("\u{2197}", Style::default().fg(theme::INDIGO))),
            title: "a config file by its path...".to_owned(),
            tone: Some(theme::INDIGO_LIGHT),
            detail: "any mirrord config on this computer".to_owned(),
            keywords: "load open file path import".to_owned(),
            ..Choice::default()
        }])
        .collect()
}

/// The folder step's "run again" rows, newest first: which folders, against what, how, and when.
fn run_again_choices(runs: &[RunRecipe]) -> Vec<Choice> {
    let now = Timestamp::now().as_second();
    runs.iter()
        .enumerate()
        .map(|(index, run)| {
            let names: Vec<String> = run
                .services
                .iter()
                .filter_map(|service| Path::new(&service.dir).file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .collect();
            let targets: Vec<&str> = run
                .services
                .iter()
                .map(|service| {
                    service
                        .target
                        .as_deref()
                        .and_then(|target| target.split(" (").next())
                        .unwrap_or("no target")
                })
                .collect();
            Choice {
                id: format!("{RUN_AGAIN}{index}"),
                section: Some("run again"),
                mark: Some(Span::styled("\u{21bb}", Style::default().fg(theme::MINT))),
                title: names.join(" + "),
                tone: Some(theme::LAVENDER),
                detail: format!(
                    "{} \u{b7} {}",
                    targets.join(", "),
                    started_from(run.start_from.as_deref())
                ),
                note: vec![Span::styled(ago(now - run.ran_at), theme::muted())],
                keywords: run
                    .services
                    .iter()
                    .map(|service| service.dir.as_str())
                    .chain(targets.iter().copied())
                    .collect::<Vec<_>>()
                    .join(" "),
                pickable: false,
            }
        })
        .collect()
}

/// How a remembered run started its config, in a few words.
fn started_from(id: Option<&str>) -> String {
    match id {
        Some("made:mine") => "only my requests".to_owned(),
        Some("made:mine+db") => "with databases branched".to_owned(),
        Some("made:takeover") => "took over all traffic".to_owned(),
        Some("made:none") | None => "no incoming traffic".to_owned(),
        Some(other) => other
            .strip_prefix("file:")
            .and_then(|path| Path::new(path).file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| other.to_owned()),
    }
}

/// How long ago something was, `seconds` back: `just now`, `5m ago`, `3h ago`, `2d ago`.
fn ago(seconds: i64) -> String {
    match seconds {
        ..60 => "just now".to_owned(),
        60..3600 => format!("{}m ago", seconds / 60),
        3600..86400 => format!("{}h ago", seconds / 3600),
        _ => format!("{}d ago", seconds / 86400),
    }
}

/// A name for a new config in `dir` that no file in its `.mirrord` has yet: `mirrord`,
/// `mirrord-2`, ...
fn free_config_name(dir: &Path) -> String {
    let taken = |name: &str| dir.join(".mirrord").join(format!("{name}.json")).exists();
    std::iter::once("mirrord".to_owned())
        .chain((2..).map(|number| format!("mirrord-{number}")))
        .find(|name| !taken(name))
        .unwrap_or_default()
}

/// The file name a typed config name is saved as: characters other than letters, digits, `-`,
/// `_` and `.` become `-`, and `.json` is added unless it ends in `.json`, `.yaml`, `.yml` or
/// `.toml`. `None` for an empty name.
fn config_file_name(name: &str) -> Option<String> {
    let name: String = name
        .trim()
        .chars()
        .map(|char| {
            if char.is_ascii_alphanumeric() || "-_.".contains(char) {
                char
            } else {
                '-'
            }
        })
        .collect();
    if name.is_empty() {
        return None;
    }
    let has_extension = [".json", ".yaml", ".yml", ".toml"]
        .iter()
        .any(|extension| name.ends_with(extension));
    Some(if has_extension {
        name
    } else {
        format!("{name}.json")
    })
}

/// Puts `edits` over `base` the way a JSON merge patch does: maps merge key by key, anything else
/// is replaced.
fn merge(base: &mut Value, edits: Value) {
    match (base, edits) {
        (Value::Object(base), Value::Object(edits)) => {
            for (key, value) in edits {
                merge(base.entry(key).or_insert(Value::Null), value);
            }
        }
        (base, edits) => *base = edits,
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
        let auto_advance = !services.is_empty();

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
                )
                .with_levels(),
            ),
            targets: SelectList::new(Picks::Many, "type to search targets"),
            picked: Vec::new(),
            auto_picked: BTreeSet::new(),
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
            naming: None,
            working: None,
            loading: None,
            start_list: None,
            launch: None,
            log_open: false,
            run_hit: None,
            pending_targets: Vec::new(),
            auto_advance,
            filled: false,
            notice: None,
            ran_again_rows_set: false,
            problem: None,
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        if !self.ran_again_rows_set {
            self.ran_again_rows_set = true;
            self.folders.set_top(run_again_choices(&history::runs()));
        }
        if let Some(launch) = self.launch.as_mut().filter(|_| self.log_open) {
            let [log_area, legend_area] =
                Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(area);
            launch.draw(frame, log_area, true);
            let legend = if launch.running() {
                vec![
                    Hint::new("x", "stop"),
                    Hint::new("esc", "back to the steps (keeps running)"),
                    Hint::new("\u{2191}\u{2193}", "scroll"),
                ]
            } else {
                vec![
                    Hint::new("enter", "back to the steps"),
                    Hint::new("\u{2191}\u{2193}", "scroll"),
                ]
            };
            frame.render_widget(Paragraph::new(hints::legend(&legend)), legend_area);
            return;
        }
        self.rank_services();
        self.pick_pending_targets();
        self.auto_pick_targets();
        self.auto_advance_when_known();
        if self.step == Step::Config {
            self.ensure_working();
            self.fill_in_settings();
        }
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
        if let Some(list) = &mut self.start_list {
            let height = area.height.saturating_sub(4).min(22);
            let inner = dialog(
                frame,
                centered(area, Constraint::Length(96), Constraint::Length(height)),
                "Start from",
            );
            let [list_area, legend_area] =
                Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(inner);
            let chosen = self.config_choice.clone().unwrap_or_default();
            list.draw(frame, list_area, &|id| id == chosen);
            frame.render_widget(
                Paragraph::new(hints::legend(&[
                    Hint::new("enter", "use it"),
                    Hint::new("type", "search: db, kafka, steal"),
                    Hint::new("esc", "cancel"),
                ])),
                legend_area,
            );
        }
        if let Some(input) = &mut self.loading {
            let inner = dialog(
                frame,
                centered(area, Constraint::Length(80), Constraint::Length(7)),
                "Load a config file",
            );
            let [field_area, _, legend_area] = Layout::vertical([
                Constraint::Length(3),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .areas(inner);
            input.render(
                frame,
                field_area,
                FieldLook {
                    placeholder: "~/code/shop/.mirrord/mirrord.json",
                    label: Some("path"),
                    focused: true,
                    ..FieldLook::default()
                },
            );
            frame.render_widget(
                Paragraph::new(hints::legend(&[
                    Hint::new("enter", "load it"),
                    Hint::new("esc", "cancel"),
                ])),
                legend_area,
            );
        }
        if let Some((dir, input)) = &mut self.naming {
            let inner = dialog(
                frame,
                centered(area, Constraint::Length(72), Constraint::Length(8)),
                "Name the config",
            );
            let [field_area, saved_area, _, legend_area] = Layout::vertical([
                Constraint::Length(3),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .areas(inner);
            let saved = format!(
                "saved as {}/.mirrord/{}.json",
                suggest::compress_home(&dir.display().to_string()),
                input.value().trim()
            );
            input.render(
                frame,
                field_area,
                FieldLook {
                    placeholder: "a short name, such as steal or with-db",
                    label: Some("name"),
                    focused: true,
                    ..FieldLook::default()
                },
            );
            frame.render_widget(
                Paragraph::new(Line::styled(format!(" {saved}"), theme::muted())),
                saved_area,
            );
            frame.render_widget(
                Paragraph::new(hints::legend(&[
                    Hint::new("enter", "open every option"),
                    Hint::new("esc", "cancel"),
                ])),
                legend_area,
            );
        }
        if let Some((dir, _)) = &self.editor {
            let name = self.service(dir).map(Service::name).unwrap_or_default();
            let inner = dialog(
                frame,
                centered(area, Constraint::Length(72), Constraint::Length(7)),
                &format!("Command for {name}"),
            );
            let [field_area, _, legend_area] = Layout::vertical([
                Constraint::Length(3),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .areas(inner);
            if let Some((_, input)) = &mut self.editor {
                input.render(
                    frame,
                    field_area,
                    FieldLook {
                        prefix: "$",
                        placeholder: "the command to run, such as npm run dev",
                        label: Some("command"),
                        focused: true,
                        ..FieldLook::default()
                    },
                );
            }
            frame.render_widget(
                Paragraph::new(hints::legend(&[
                    Hint::new("enter", "keep"),
                    Hint::new("esc", "cancel"),
                ])),
                legend_area,
            );
        }
    }

    fn handle_event(&mut self, event: Event) -> ControlFlow<(), Event> {
        // The user has the wheel from their first key on.
        if matches!(event, Event::Key(_)) {
            self.auto_advance = false;
            self.notice = None;
        }
        if let Some(launch) = self.launch.as_mut().filter(|_| self.log_open) {
            let Event::Key(key) = event else {
                return ControlFlow::Break(());
            };
            match key.code {
                // While it runs, Esc only hides the log: the steps come back and the run goes on.
                KeyCode::Esc if launch.running() => self.log_open = false,
                // Once it stopped, Esc or Enter closes it (the pane removes its plan file).
                KeyCode::Esc | KeyCode::Enter if !launch.running() => {
                    if launch.handle_key(KeyEvent::from(KeyCode::Esc)) {
                        self.launch = None;
                    }
                    self.log_open = false;
                }
                _ => {
                    launch.handle_key(key);
                }
            }
            return ControlFlow::Break(());
        }
        if self.launch.is_some()
            && let Event::Key(key) = &event
            && key.code == KeyCode::Char('l')
            && key.modifiers.contains(KeyModifiers::CONTROL)
        {
            self.log_open = true;
            return ControlFlow::Break(());
        }

        if let Some(list) = &mut self.start_list {
            let outcome = match &event {
                Event::Key(key) => list.handle_key(key),
                Event::Mouse(mouse) => list.handle_mouse(mouse),
                Event::Paste(text) => {
                    list.paste(text);
                    ListEvent::Moved
                }
                _ => ListEvent::Nothing,
            };
            self.on_start_list(outcome);
            return ControlFlow::Break(());
        }

        if let Some(input) = &mut self.loading {
            match &event {
                Event::Paste(text) => input.paste(text),
                Event::Key(key) => match key.code {
                    KeyCode::Enter => {
                        let path = PathBuf::from(suggest::expand_home(input.value().trim()));
                        self.loading = None;
                        self.load_config(path);
                    }
                    KeyCode::Esc => self.loading = None,
                    _ => {
                        input.handle_key(key);
                    }
                },
                _ => {}
            }
            return ControlFlow::Break(());
        }

        if let Some((dir, input)) = &mut self.naming {
            match &event {
                Event::Paste(text) => input.paste(text),
                Event::Key(key) => match key.code {
                    KeyCode::Enter => {
                        let (dir, name) = (dir.clone(), input.value().to_owned());
                        self.naming = None;
                        self.save_working_as(dir, &name);
                    }
                    KeyCode::Esc => self.naming = None,
                    _ => {
                        input.handle_key(key);
                    }
                },
                _ => {}
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
            Event::Paste(text) => match self.list_mut() {
                Some(list) => {
                    list.paste(text);
                    true
                }
                None => self.working.as_mut().is_some_and(|working| {
                    working.handle_event(Event::Paste(text.clone())).is_break()
                }),
            },
            Event::Mouse(mouse) => {
                let at = Position::new(mouse.column, mouse.row);
                let step = self
                    .step_hits
                    .iter()
                    .find(|(area, _)| area.contains(at))
                    .map(|(_, step)| *step)
                    .filter(|_| mouse.kind == MouseEventKind::Down(MouseButton::Left));
                let on_run_chip = mouse.kind == MouseEventKind::Down(MouseButton::Left)
                    && self.run_hit.is_some_and(|hit| hit.contains(at));
                if on_run_chip {
                    match self.launch {
                        Some(_) => self.log_open = true,
                        None => self.run(),
                    }
                    true
                } else if let Some(step) = step {
                    self.go_to(step);
                    true
                } else if let Some(id) = self.tray.clicked(mouse) {
                    match self.step {
                        Step::Folders => self.toggle_folder(&id),
                        Step::Targets => self.toggle_target(&id),
                        Step::Config => {}
                    }
                    true
                } else if let Some(list) = self.list_mut() {
                    let event = list.handle_mouse(mouse);
                    self.on_list(event)
                } else {
                    self.working.as_mut().is_some_and(|working| {
                        working.handle_event(Event::Mouse(*mouse)).is_break()
                    })
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

    fn unsaved_changes(&self) -> Option<String> {
        // A new config is this run's, remembered with it for "run again"; only edits to a saved
        // file would be lost on quitting.
        self.working
            .as_ref()
            .filter(|working| working.is_dirty() && !working.is_scratch())
            .map(|working| format!("Run: edits to {} are not saved.", working.path().display()))
    }

    fn save_changes(&mut self) -> Result<(), String> {
        self.working
            .as_mut()
            .map_or(Ok(()), ConfigScreen::save_changes)
    }

    fn hints(&self) -> Vec<Hint> {
        if let Some(launch) = self.launch.as_ref().filter(|_| self.log_open) {
            return if launch.running() {
                vec![
                    Hint::new("x", "stop"),
                    Hint::new("esc", "back to the steps"),
                    Hint::new("\u{2191}/\u{2193}", "scroll"),
                    Hint::new("G", "follow"),
                ]
            } else {
                vec![
                    Hint::new("enter/esc", "back to the steps"),
                    Hint::new("\u{2191}/\u{2193}", "scroll"),
                ]
            };
        }
        if self.editor.is_some() || self.naming.is_some() || self.loading.is_some() {
            return vec![Hint::new("enter", "keep"), Hint::new("esc", "cancel")];
        }
        if self.picker.is_some() {
            return vec![Hint::new("enter", "pick"), Hint::new("esc", "cancel")];
        }
        if self.step == Step::Config {
            let mut hints = vec![
                Hint::new("ctrl+r", "run"),
                Hint::new("ctrl+o", "start from"),
                Hint::new("ctrl+s", "save"),
            ];
            if let Some(working) = &self.working {
                hints.extend(working.hints());
            }
            return hints;
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
    fn edits_go_over_what_the_preset_adds() {
        let mut preset = json!({ "feature": { "db_branches": [{ "type": "pg" }] } });
        merge(
            &mut preset,
            json!({ "feature": { "network": { "incoming": "steal" } }, "agent": { "ttl": 30 } }),
        );
        assert_eq!(
            preset,
            json!({
                "feature": { "db_branches": [{ "type": "pg" }], "network": { "incoming": "steal" } },
                "agent": { "ttl": 30 }
            })
        );
    }

    #[test]
    fn typed_names_become_safe_config_file_names() {
        assert_eq!(config_file_name("with-db").as_deref(), Some("with-db.json"));
        assert_eq!(
            config_file_name(" steal it ").as_deref(),
            Some("steal-it.json")
        );
        assert_eq!(config_file_name("team.yaml").as_deref(), Some("team.yaml"));
        assert_eq!(config_file_name("   "), None);
    }

    #[test]
    fn steps_go_forward_and_back_in_order() {
        assert_eq!(Step::Folders.next(), Some(Step::Targets));
        assert_eq!(Step::Config.next(), None);
        assert_eq!(Step::Config.previous(), Some(Step::Targets));
        assert_eq!(Step::Folders.previous(), None);
    }
}
