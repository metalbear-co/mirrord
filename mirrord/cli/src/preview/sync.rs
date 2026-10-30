//! Code sync for `mirrord preview sync`: pushes the local directories of a service's `sync` into
//! its preview pod whole each time the operator pairs the CLI with the pod's sidecar, and under
//! `--watch` again on each change in between.

use std::{
    future::Future,
    ops::Not,
    path::{Component, Path, PathBuf},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use mirrord_config::target::Target;
use mirrord_operator::{
    client::{
        MaybeClientCert, OperatorApi,
        error::OperatorApiError,
        preview_sync::{
            PackedEntry, PreviewSyncConnection, PreviewSyncError, RuleSources, SyncEvent,
            TreeListing, UpgradedConnection,
        },
    },
    crd::preview::{PreviewSyncConfig, PreviewSyncEntry, PreviewSyncInstall},
    sync::{
        AppReport, InstallReport, InstallState, PushMode, RejectReason, SidecarState,
        SupervisorReport, UnpairReason,
    },
};
use mirrord_progress::{Progress, ProgressTracker};
use mirrord_up::{InstallConfig, RunType, SubprocessCfg, SyncEntry};
use notify::{
    EventKind, RecommendedWatcher, RecursiveMode, Watcher,
    event::{MetadataKind, ModifyKind},
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{Notify, watch},
};

use crate::error::{CliError, CliResult};

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
// Tests read pushes with `entries[&index][path]`; a panic just fails the test.
mod tests;

/// How long the files must stay unchanged before `--watch` pushes them.
const WATCH_DEBOUNCE: Duration = Duration::from_millis(300);

/// The longest that `--watch` waits for the files to stay unchanged before it pushes anyway, so
/// that a process that keeps writing, such as a dev server's log, can't hold back every push.
const WATCH_MAX_DELAY: Duration = Duration::from_secs(2);

/// The code sync of a `mirrord preview sync` service: the `sync` entries that the CLI pushes, and
/// the sync settings of the session.
pub(super) struct ServiceSync {
    /// The `sync` entries of the service, by their index in `config.entries`.
    pub(super) entries: Vec<SyncEntry>,

    pub(super) config: PreviewSyncConfig,
}

impl ServiceSync {
    /// The code sync of `service`, or `None` when it has no `sync`.
    ///
    /// Translates each `install.on_change` glob into the container, under the `remote` of the
    /// entry whose `local` holds it, and refuses the settings that a sync preview can't run,
    /// before anything changes in the cluster.
    pub(super) fn of_service(service: &SubprocessCfg) -> CliResult<Option<Self>> {
        if service.sync.is_empty() {
            return Ok(None);
        }

        // A Windows tree has no Unix permissions, so a push would drop the executable bit of
        // every script, and the translated `on_change` globs would hold `\`, which the pod
        // doesn't read as a separator.
        if cfg!(windows) {
            return Err(CliError::PreviewSyncOnWindows);
        }

        if service.run.r#type == RunType::Container {
            return Err(CliError::PreviewSyncContainerRun);
        }

        if service.run.pre_command.is_empty().not() {
            return Err(CliError::PreviewSyncPreCommand);
        }

        // A session that starts idle has no pod to push into, and the pod that the first request
        // wakes would wait for a push that nobody sends.
        let preview = &service.config.feature.preview;
        if preview.idle.start_idle {
            return Err(CliError::PreviewSyncStartIdle);
        }

        // Only one pod at a time can write the code volume.
        if preview.replicas != 1 {
            return Err(CliError::PreviewSyncReplicas(preview.replicas));
        }

        // A CronJob creates its pods on a schedule, after the CLI has pushed.
        if matches!(service.config.target.path, Some(Target::CronJob(_))) {
            return Err(CliError::PreviewSyncCronJobTarget);
        }

        let install = match &service.install {
            None => None,
            Some(InstallConfig {
                command,
                on_change,
                persist,
            }) => {
                let on_change = on_change
                    .iter()
                    .map(|glob| {
                        container_glob(glob, &service.sync)
                            .ok_or_else(|| CliError::PreviewSyncOnChangeOutsideLocal(glob.clone()))
                    })
                    .collect::<CliResult<_>>()?;

                Some(PreviewSyncInstall {
                    command: command.clone(),
                    on_change,
                    persist: persist.clone(),
                })
            }
        };

        let config = PreviewSyncConfig {
            entries: service
                .sync
                .iter()
                .map(|entry| PreviewSyncEntry {
                    remote: entry.remote.clone(),
                })
                .collect(),
            run_command: service.run.command.clone(),
            working_dir: service.run.remote_directory.clone(),
            install,
            restart_on_change: service.restart_on_change,
        };
        config.check()?;

        Ok(Some(Self {
            entries: service.sync.clone(),
            config,
        }))
    }
}

/// The absolute glob in the container that matches what the local `glob` matches, or `None`
/// when `glob` doesn't lie under the `local` of any entry in `entries`. When the `local`s of
/// several entries hold `glob`, the deepest one decides, so that a glob under a nested entry
/// points into that entry's `remote`.
fn container_glob(glob: &Path, entries: &[SyncEntry]) -> Option<String> {
    let glob = lexically_normal(glob);

    entries
        .iter()
        .filter_map(|entry| {
            let local = lexically_normal(&entry.local);
            let under_local = glob.strip_prefix(&local).ok()?;

            // A glob equal to the `local` would match the `remote` directory itself, which a
            // push never changes.
            (under_local.as_os_str().is_empty().not())
                .then(|| (local.components().count(), &entry.remote, under_local))
        })
        .max_by_key(|(depth, _, _)| *depth)
        .map(|(_, remote, under_local)| format!("{remote}/{}", under_local.to_string_lossy()))
}

/// `path` without its `.` components, and with each `..` component removed together with the
/// component before it, without reading the file system.
fn lexically_normal(path: &Path) -> PathBuf {
    let mut normal = PathBuf::new();

    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normal.pop();
            }
            component => normal.push(component),
        }
    }

    normal
}

/// What the sync of a session did so far, which `mirrord preview sync` reads while it waits for the
/// session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct SyncActivity {
    /// The generation that the first push made, once the volume holds it.
    pub(super) first_push: Option<u64>,
}

/// Opens a sync connection of the session, as many times as the sync needs one. A failure says
/// whether a new connection may succeed ([`Stop::Disconnected`]) or not ([`Stop::Fatal`]).
pub(super) type Connect<S> = Box<
    dyn FnMut() -> Pin<Box<dyn Future<Output = Result<PreviewSyncConnection<S>, Stop>> + Send>>
        + Send,
>;

/// The first wait before `--watch` connects again after the connection to the operator broke.
const RECONNECT_DELAY: Duration = Duration::from_secs(1);

/// The longest wait before `--watch` connects again, which the wait doubles up to.
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(30);

/// The code sync of one service of `mirrord preview sync`: connects to the session's `sync`
/// websocket, pushes the service's `sync` entries whenever the operator pairs the connection with
/// the preview pod's sidecar, and with `--watch` pushes them again on every change in between.
///
/// A push that fails with an I/O error is retried once, with a new connection and a full push,
/// and a rejection that would repeat fails the sync right away. A broken connection to the
/// operator is retried once too, and under `--watch` again and again, with a growing wait, for as
/// long as the run lasts. The one retry becomes available again once a push is applied.
///
/// `S` is the stream under the connection's websocket, which tests replace.
pub(super) struct SyncDriver<S> {
    /// The `sync` entries of the service, by their index in the session's `spec.sync`.
    entries: Arc<[SyncEntry]>,

    connect: Connect<S>,

    /// Watches the entries' trees under `--watch`, from before the first listing, so that it sees
    /// every change that a push doesn't hold.
    watcher: Option<TreeWatcher>,

    activity: watch::Sender<SyncActivity>,
    progress: ProgressTracker,

    /// The id of the next push. The ids of one CLI start at a random number, so that they
    /// don't repeat the ids of an earlier CLI, whose pushes the operator and the sidecar may
    /// still hold when this one takes over.
    next_push_id: u64,

    /// How many pushes the sidecar applied, so that the one retry becomes available again after
    /// one.
    applied_pushes: u64,

    pairing: Pairing,

    /// Where each entry's rules come from, found by the first listing.
    sources: Option<Arc<[RuleSources]>>,

    sidecar: Option<SidecarState>,
    supervisor: SupervisorReport,
}

/// Whether the operator pairs the connection with the preview pod's sidecar, so that the CLI can
/// push.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pairing {
    /// Waiting for `Paired`: the first one, or the next one after the operator unpaired the
    /// connection because the sidecar went or a `mirrord exec` session paused the preview.
    Waiting,

    /// Waiting for `Paired` while the preview is idle. The first change asks for a wake.
    Idle,

    /// Waiting for `Paired` after asking the operator to wake the idle preview.
    Waking,

    Paired,
}

/// How a push ended, when it didn't stop the sync.
enum Pushed {
    /// The sidecar applied the push, and the volume holds this generation.
    Applied(u64),

    /// The operator unpaired the connection before the sidecar answered, so the sidecar may or
    /// may not have applied the push, and the CLI pushes in full after the next `Paired`.
    Unpaired(UnpairReason),
}

/// Why the sync stopped.
pub(super) enum Stop {
    /// The connection to the operator broke, or couldn't be made.
    Disconnected(Box<CliError>),

    /// A failure that a retry may not repeat: the sidecar failed to apply a push with an I/O
    /// error or wasn't ready for it, or the CLI couldn't list or pack a tree.
    Transient(Box<CliError>),

    /// An error that would repeat, so the sync fails right away.
    Fatal(Box<CliError>),
}

impl From<PreviewSyncError> for Stop {
    fn from(error: PreviewSyncError) -> Self {
        match error {
            PreviewSyncError::WebSocket(_) | PreviewSyncError::Closed => {
                Stop::Disconnected(Box::new(error.into()))
            }
            PreviewSyncError::Rejected(RejectReason::Io(_) | RejectReason::NotReady) => {
                Stop::Transient(Box::new(error.into()))
            }
            PreviewSyncError::Unpaired(UnpairReason::Replaced) => {
                Stop::Fatal(Box::new(CliError::PreviewSyncReplaced))
            }
            error => Stop::Fatal(Box::new(error.into())),
        }
    }
}

impl SyncDriver<UpgradedConnection> {
    /// The driver of the sync of the session `name` in `namespace`, which pushes `entries`, the
    /// service's `sync` entries in the order of the session's `spec.sync`. With `watch_files`, it
    /// starts watching the entries' trees here.
    pub(super) fn new(
        operator_api: Arc<OperatorApi<MaybeClientCert>>,
        namespace: String,
        name: String,
        entries: Vec<SyncEntry>,
        watch_files: bool,
        progress: ProgressTracker,
    ) -> CliResult<(Self, watch::Receiver<SyncActivity>)> {
        let connect: Connect<UpgradedConnection> = Box::new(move || {
            let operator_api = operator_api.clone();
            let namespace = namespace.clone();
            let name = name.clone();
            Box::pin(async move {
                operator_api
                    .connect_preview_sync(&namespace, &name)
                    .await
                    .map_err(|error| {
                        // The operator refuses the connection of a session that is gone (404) or
                        // that doesn't run (409), which no new connection changes.
                        let gone = matches!(
                            &error,
                            OperatorApiError::KubeError {
                                error: kube::Error::Api(status),
                                ..
                            } if matches!(status.code, 404 | 409)
                        );
                        let error = Box::new(error.into());
                        if gone {
                            Stop::Fatal(error)
                        } else {
                            Stop::Disconnected(error)
                        }
                    })
            })
        });

        Self::with_connect(connect, entries.into(), watch_files, progress)
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin + Send + 'static> SyncDriver<S> {
    fn with_connect(
        connect: Connect<S>,
        entries: Arc<[SyncEntry]>,
        watch_files: bool,
        progress: ProgressTracker,
    ) -> CliResult<(Self, watch::Receiver<SyncActivity>)> {
        let watcher = watch_files
            .then(|| TreeWatcher::new(&entries))
            .transpose()?;
        let (activity, activity_rx) = watch::channel(SyncActivity::default());

        let driver = Self {
            entries,
            connect,
            watcher,
            activity,
            progress,
            next_push_id: rand::random(),
            applied_pushes: 0,
            pairing: Pairing::Waiting,
            sources: None,
            sidecar: None,
            supervisor: SupervisorReport::default(),
        };

        Ok((driver, activity_rx))
    }

    /// Syncs until the sync fails, or until another CLI takes it over, which fails it with
    /// [`CliError::PreviewSyncReplaced`], and returns the error. It never ends on its own: the
    /// caller drops it once it no longer needs the sync, such as when the session is ready and the
    /// run has no `--watch`.
    pub(super) async fn run(mut self) -> CliError {
        let mut may_retry = true;
        let mut reconnect_delay = RECONNECT_DELAY;

        loop {
            let applied_before = self.applied_pushes;
            let stop = match (self.connect)().await {
                Ok(connection) => self.serve(connection).await,
                Err(stop) => stop,
            };

            if self.applied_pushes > applied_before {
                may_retry = true;
                reconnect_delay = RECONNECT_DELAY;
            }

            match stop {
                // Under `--watch`, the run lasts as long as the developer wants, and an operator
                // restart or upgrade breaks the connection meanwhile.
                Stop::Disconnected(error) if self.watcher.is_some() => {
                    self.progress.warning(&format!(
                        "{}; reconnecting in {} seconds to push the code again",
                        with_causes(&error),
                        reconnect_delay.as_secs()
                    ));
                    tokio::time::sleep(reconnect_delay).await;
                    reconnect_delay = (reconnect_delay * 2).min(MAX_RECONNECT_DELAY);
                }
                Stop::Disconnected(error) | Stop::Transient(error) if may_retry => {
                    may_retry = false;
                    self.progress.warning(&format!(
                        "{}; reconnecting to push the code again",
                        with_causes(&error)
                    ));
                }
                Stop::Disconnected(error) | Stop::Transient(error) | Stop::Fatal(error) => {
                    return *error;
                }
            }
        }
    }

    /// Serves one connection: pushes the trees whole whenever the operator pairs it, and with
    /// `--watch` again on each change in between. Returns why it stopped.
    async fn serve(&mut self, mut connection: PreviewSyncConnection<S>) -> Stop {
        self.pairing = Pairing::Waiting;

        loop {
            let batch = async {
                match &mut self.watcher {
                    Some(watcher) => watcher.next_batch().await,
                    None => std::future::pending().await,
                }
            };

            let result = tokio::select! {
                event = connection.next_event() => match event {
                    Ok(SyncEvent::Paired { .. }) => {
                        self.pairing = Pairing::Paired;
                        self.push_full(&mut connection, false).await
                    }
                    Ok(SyncEvent::Unpaired(UnpairReason::Replaced)) => {
                        return Stop::Fatal(Box::new(CliError::PreviewSyncReplaced));
                    }
                    Ok(SyncEvent::Unpaired(reason)) => {
                        self.unpaired(reason);
                        Ok(())
                    }
                    Ok(event) => {
                        self.observe(&event);
                        Ok(())
                    }
                    Err(error) => Err(error.into()),
                },
                () = batch => match self.pairing {
                    Pairing::Paired => self.push_full(&mut connection, true).await,
                    Pairing::Idle => {
                        self.pairing = Pairing::Waking;
                        self.progress.info("waking the idle preview to push the change");
                        connection.wake().await.map_err(Stop::from)
                    }
                    // The next `Paired` brings a full push, which holds the change.
                    Pairing::Waiting | Pairing::Waking => Ok(()),
                },
            };

            if let Err(stop) = result {
                return stop;
            }
        }
    }

    /// Lists and pushes every entry whole, which makes each `remote` a mirror of its tree.
    ///
    /// When the trees can't be packed, such as a file that the CLI can't read, the sync stops,
    /// unless a change started the push: a file that a tool removes while the CLI reads the tree
    /// fails the pack, and its removal is a change, which starts the next push.
    async fn push_full(
        &mut self,
        connection: &mut PreviewSyncConnection<S>,
        after_change: bool,
    ) -> Result<(), Stop> {
        let entries = self.entries.clone();
        let sources = self.sources.clone();
        let full = tokio::task::spawn_blocking(move || list_and_pack(&entries, sources))
            .await
            .map_err(|error| Stop::Fatal(Box::new(CliError::PreviewSyncFailed(Box::new(error)))))?;

        let full = match full {
            Ok(full) => full,
            Err(error) if after_change => {
                self.progress.warning(&with_causes(&error));
                return Ok(());
            }
            Err(error) => return Err(Stop::Transient(Box::new(error))),
        };
        self.sources = Some(full.sources);

        match self.push(connection, PushMode::Full, full.packed).await? {
            Pushed::Applied(generation) => {
                if self.activity.borrow().first_push.is_none() {
                    self.activity
                        .send_modify(|activity| activity.first_push = Some(generation));
                }
                Ok(())
            }
            Pushed::Unpaired(reason) => {
                self.unpaired(reason);
                Ok(())
            }
        }
    }

    /// Sends `packed` as one push in `mode`.
    async fn push(
        &mut self,
        connection: &mut PreviewSyncConnection<S>,
        mode: PushMode,
        packed: Vec<PackedEntry>,
    ) -> Result<Pushed, Stop> {
        let push_id = self.next_push_id;
        self.next_push_id = self.next_push_id.wrapping_add(1);

        let result = connection
            .push(push_id, mode, packed, |event| self.observe(&event))
            .await;

        let generation = match result {
            Ok(generation) => generation,
            Err(PreviewSyncError::Unpaired(reason)) if reason != UnpairReason::Replaced => {
                return Ok(Pushed::Unpaired(reason));
            }
            Err(error) => return Err(error.into()),
        };

        self.applied_pushes += 1;
        self.progress
            .info(&format!("pushed the local code (generation {generation})"));

        Ok(Pushed::Applied(generation))
    }

    /// Waits for the next `Paired` after the operator unpaired the connection for `reason`.
    /// The supervisor's last report described a pod that may be gone, so it no longer counts.
    fn unpaired(&mut self, reason: UnpairReason) {
        self.pairing = match reason {
            UnpairReason::Idle => Pairing::Idle,
            _ => Pairing::Waiting,
        };
        self.supervisor = SupervisorReport::default();
        self.progress
            .info(&format!("waiting to push again: {reason}"));
    }

    /// Reports `event`, and records what it says about the preview pod.
    fn observe(&mut self, event: &SyncEvent) {
        match event {
            SyncEvent::Sidecar(state) => {
                if self.sidecar.as_ref() == Some(state) {
                    return;
                }
                self.progress
                    .info(&format!("sync sidecar: {state}"));
                self.sidecar = Some(state.clone());

                // The supervisor's last report described a pod that may be gone.
                if *state == SidecarState::Disconnected {
                    self.supervisor = SupervisorReport::default();
                }
            }
            SyncEvent::Supervisor(report) => {
                if *report == self.supervisor {
                    return;
                }
                if report.install != self.supervisor.install
                    && let Some(message) = describe_install(&report.install)
                {
                    self.progress.info(&message);
                }
                if report.app.state != self.supervisor.app.state {
                    self.progress
                        .info(&format!("app: {}", describe_app(&report.app)));
                }
                self.supervisor = report.clone();
            }
            SyncEvent::Paired { .. } | SyncEvent::Unpaired(_) => {}
        }
    }
}

/// The entries of a full push, with where their rules come from.
struct FullPack {
    packed: Vec<PackedEntry>,

    /// Where each entry's rules come from.
    sources: Arc<[RuleSources]>,
}

/// Lists and packs every entry whole. Finds where each entry's rules come from, unless
/// `sources` holds them from an earlier listing.
fn list_and_pack(
    entries: &[SyncEntry],
    sources: Option<Arc<[RuleSources]>>,
) -> CliResult<FullPack> {
    let sources = match sources {
        Some(sources) => sources,
        None => entries
            .iter()
            .map(|entry| {
                RuleSources::new(&entry.local).map_err(|error| CliError::PreviewSyncReadFailed {
                    path: entry.local.clone(),
                    error,
                })
            })
            .collect::<Result<Vec<_>, _>>()?
            .into(),
    };

    let packed = entries
        .iter()
        .zip(sources.iter())
        .enumerate()
        .map(|(index, (entry, entry_sources))| {
            let failed = |error| CliError::PreviewSyncReadFailed {
                path: entry.local.clone(),
                error,
            };
            TreeListing::new_watching(entry_sources, &entry.ignore, &mut |_| Ok(()))
                .and_then(|listing| listing.pack(index as u32, &entry.local))
                .map_err(failed)
        })
        .collect::<Result<_, _>>()?;

    Ok(FullPack { packed, sources })
}

/// Watches the trees of the pushed entries, each one recursively.
struct TreeWatcher {
    /// Kept for its watches, which end when it drops.
    _watcher: RecommendedWatcher,

    /// Counts the events, so that a batch waits until they stop.
    events: Arc<AtomicU64>,

    /// The count of events when the last batch ended.
    batched: u64,

    /// Notified on each event.
    changed: Arc<Notify>,

    /// When the batch that [`Self::next_batch`] waits for must end, even if events keep coming.
    batch_ends_by: Option<tokio::time::Instant>,
}

impl TreeWatcher {
    /// Watches the `local` directory of each of `entries`.
    fn new(entries: &[SyncEntry]) -> CliResult<Self> {
        let events = Arc::new(AtomicU64::new(0));
        let changed = Arc::new(Notify::new());

        let mut watcher = notify::recommended_watcher({
            let events = events.clone();
            let changed = changed.clone();
            move |event: notify::Result<notify::Event>| {
                // An error may hide a change, which only a new listing finds.
                if event.is_ok_and(|event| changes_the_tree(&event.kind).not()) {
                    return;
                }
                events.fetch_add(1, Ordering::Relaxed);
                changed.notify_one();
            }
        })
        .map_err(|error| CliError::PreviewSyncFailed(Box::new(error)))?;

        for local in entries.iter().map(|entry| &entry.local) {
            watcher
                .watch(local, RecursiveMode::Recursive)
                .map_err(|error| match error.kind {
                    // On Linux, a recursive watch takes one inotify watch per directory,
                    // including those of `node_modules`.
                    notify::ErrorKind::MaxFilesWatch => CliError::PreviewSyncFailed(
                        format!(
                            "watching `{}` needs more file watches than the system allows; \
                             raise `fs.inotify.max_user_watches`",
                            local.display()
                        )
                        .into(),
                    ),
                    _ => CliError::PreviewSyncFailed(Box::new(error)),
                })?;
        }

        Ok(Self {
            _watcher: watcher,
            events,
            batched: 0,
            changed,
            batch_ends_by: None,
        })
    }

    fn events(&self) -> u64 {
        self.events.load(Ordering::Relaxed)
    }

    /// Waits for the next change, then until the files stay unchanged, so that one save of
    /// several files makes one push.
    ///
    /// Cancel-safe: a call that is dropped before it returns leaves what it waited for to the
    /// next call.
    async fn next_batch(&mut self) {
        let ends_by = match self.batch_ends_by {
            Some(ends_by) => ends_by,
            None => {
                while self.events() == self.batched {
                    self.changed.notified().await;
                }
                let ends_by = tokio::time::Instant::now() + WATCH_MAX_DELAY;
                self.batch_ends_by = Some(ends_by);
                ends_by
            }
        };

        loop {
            let events = self.events();
            let quiet_by = (tokio::time::Instant::now() + WATCH_DEBOUNCE).min(ends_by);
            tokio::time::sleep_until(quiet_by).await;

            if self.events() == events || tokio::time::Instant::now() >= ends_by {
                self.batched = events;
                break;
            }
        }

        self.batch_ends_by = None;
    }
}

/// `error` with each of its causes, on one line, for a progress message, which shows no causes
/// on its own. A cause that the message already holds, because an error put its source into its
/// own message, isn't repeated.
pub(super) fn with_causes(error: &dyn std::error::Error) -> String {
    let mut message = error.to_string();
    let mut cause = error.source();
    while let Some(error) = cause {
        let text = error.to_string();
        if message.contains(&text).not() {
            message.push_str(": ");
            message.push_str(&text);
        }
        cause = error.source();
    }
    message
}

/// Whether an event of `kind` can change the files under a watched directory. Listing and
/// packing a tree read it, and the watcher reports those reads too, so counting them would make
/// every push start the next one. `Other` includes the watcher's notice that the kernel dropped
/// events, after which only a new listing tells what changed.
fn changes_the_tree(kind: &EventKind) -> bool {
    match kind {
        EventKind::Create(_) | EventKind::Remove(_) | EventKind::Other => true,
        EventKind::Modify(ModifyKind::Metadata(MetadataKind::AccessTime)) => false,
        EventKind::Modify(_) => true,
        EventKind::Access(_) | EventKind::Any => false,
    }
}

/// What a change of the install's report means to the developer, or `None` when it says
/// nothing new.
fn describe_install(install: &InstallReport) -> Option<String> {
    match install.state {
        InstallState::NotRun => None,
        InstallState::Failed => Some(match &install.error {
            Some(error) => format!("install: failed:\n{error}"),
            None => "install: failed".to_owned(),
        }),
        state => Some(format!("install: {state}")),
    }
}

/// The app's state in the words of `mirrord preview sync` and `mirrord preview status`.
pub(super) fn describe_app(app: &AppReport) -> String {
    let state = app.state;
    match (app.restarts, app.last_exit_code) {
        (0, _) => state.to_string(),
        (restarts, Some(code)) => {
            format!("{state}, restarted {restarts} times, last exit code {code}")
        }
        (restarts, None) => format!("{state}, restarted {restarts} times"),
    }
}
