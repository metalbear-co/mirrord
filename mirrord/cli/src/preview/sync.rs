//! Code sync for `mirrord preview sync`: makes a full push of the local directories of a
//! service's `sync` into its preview pod each time the operator pairs the CLI with the pod's
//! sidecar, and under `--watch` pushes each change in between. A full push sends the manifest of
//! each tree, then only the files that the volume lacks, and makes each `remote` a mirror of its
//! tree.

use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    io,
    ops::Not,
    path::{Component, Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};

use mirrord_config::target::Target;
use mirrord_operator::{
    client::{
        MaybeClientCert, OperatorApi,
        error::OperatorApiError,
        preview_sync::{
            PackedEntry, PreviewSyncConnection, PreviewSyncError, RuleSources, SyncEvent,
            TreeListing, UpgradedConnection, pack_changes,
        },
    },
    crd::preview::{PreviewSyncConfig, PreviewSyncEntry, PreviewSyncInstall},
    sync::{
        AppReport, InstallReport, InstallState, ManifestPath, PushMode, RejectReason, SidecarState,
        SupervisorReport, UnpairReason, rules::IgnoreRules,
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

// `mirrord preview sync` refuses to run on Windows, so its tests only run elsewhere.
#[cfg(all(test, unix))]
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
/// session, to tell progress from a stall.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct SyncActivity {
    /// The generation that the first push since the operator paired the CLI with the preview
    /// pod made, once the volume holds it. A new pairing can reach a new volume, such as an
    /// `emptyDir` of a replaced pod, whose generations start again, so each pairing has its own.
    pub(super) first_push: Option<u64>,

    /// Whether the sync does work whose length the CLI can't predict, during which no creation
    /// timeout runs: the sidecar waits for the code volume or copies the image's files to it,
    /// the CLI lists, hashes or packs the trees of a push, the push is being sent or applied, or
    /// the install runs.
    pub(super) busy: bool,

    /// Counts the reports of progress: a new state of the sidecar, of the install or of the app,
    /// a new handled push, and every applied push. A failed restart of the app isn't progress,
    /// because the supervisor retries it for as long as the app fails to start.
    pub(super) progress: u64,

    /// The generation of the last push that the preview pod's supervisor acted on, from its last
    /// report. Once it reaches `first_push`, the supervisor is done with the first push.
    pub(super) handled: Option<u64>,

    /// Whether the supervisor's last report says that the last install failed.
    pub(super) install_failed: bool,

    /// Whether the operator unpaired the CLI because a `mirrord exec` session paused the
    /// preview, so the preview pod applies no push until that session ends.
    pub(super) paused: bool,
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
/// the preview pod's sidecar, and with `--watch` pushes every change in between.
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

    /// Watches the entries' trees under `--watch`. Each listing starts watching a directory
    /// right before it reads it, so the watch sees every change that the listing doesn't hold.
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

    /// The generation that the volume holds after this CLI's last push, which an incremental
    /// push builds on.
    generation: Option<u64>,

    /// Where each entry's rules come from, found by the first listing.
    sources: Option<Arc<[RuleSources]>>,

    /// The rules of each entry's last full push, which its incremental pushes apply too, and
    /// which keep the watcher's ignored paths out of a push.
    rules: Arc<[IgnoreRules]>,

    /// The files outside the trees that the rules came from or could come from.
    rule_files: BTreeSet<PathBuf>,

    sidecar: Option<SidecarState>,
    supervisor: SupervisorReport,

    /// Whether the CLI is busy with a push, from the listing of its trees to the sidecar's answer.
    pushing: bool,
}

/// Whether the operator pairs the connection with the preview pod's sidecar, so that the CLI can
/// push.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pairing {
    /// Waiting for `Paired`: the first one, or the next one after the operator unpaired the
    /// connection because the sidecar went.
    Waiting,

    /// Waiting for `Paired` after a `mirrord exec` session paused the preview, which ends when
    /// that session ends.
    Paused,

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

    /// The sidecar rejected an incremental push because the volume holds another generation
    /// than the one it built on, so only a full push makes it a mirror again.
    StaleBase,

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
    /// creates the watcher here, and each listing then watches the directories that it reads.
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
        let watcher = watch_files.then(TreeWatcher::new).transpose()?;
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
            generation: None,
            sources: None,
            rules: Arc::new([]),
            rule_files: BTreeSet::new(),
            sidecar: None,
            supervisor: SupervisorReport::default(),
            pushing: false,
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

            // What the connection reported about the preview pod may no longer hold, such as an
            // install that ran, which would keep the creation timeout stopped while the CLI
            // can't hear of its end.
            self.sidecar = None;
            self.supervisor = SupervisorReport::default();
            self.pushing = false;
            self.publish(false);

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

    /// Serves one connection: makes a full push whenever the operator pairs it, and with
    /// `--watch` pushes the changes in between. Returns why it stopped.
    async fn serve(&mut self, mut connection: PreviewSyncConnection<S>) -> Stop {
        let mut pending = Changes::default();
        self.pairing = Pairing::Waiting;
        self.activity
            .send_if_modified(|activity| activity.first_push.take().is_some());

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
                        pending = Changes::default();
                        self.push_full(&mut connection, None).await
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
                batch = batch => {
                    pending.merge(batch, &self.entries, &self.rule_files, &self.rules);

                    match self.pairing {
                        Pairing::Paired if pending.is_empty().not() => {
                            let changes = std::mem::take(&mut pending);
                            self.push_changes(&mut connection, changes, &mut pending).await
                        }
                        Pairing::Idle if pending.is_empty().not() => {
                            self.pairing = Pairing::Waking;
                            self.progress.info("waking the idle preview to push the change");
                            connection.wake().await.map_err(Stop::from)
                        }
                        // The next `Paired` brings a full push, which holds these changes.
                        _ => Ok(()),
                    }
                }
            };

            if let Err(stop) = result {
                return stop;
            }
        }
    }

    /// Makes a full push of every entry, which makes each `remote` a mirror of its tree.
    ///
    /// When the trees can't be listed or packed, such as a file that the CLI can't read, the
    /// sync stops, unless changes started the push: then `pending` turns into a full push,
    /// which the next change starts, so that a file that a tool keeps unreadable for a while
    /// doesn't end `--watch`.
    async fn push_full(
        &mut self,
        connection: &mut PreviewSyncConnection<S>,
        pending: Option<&mut Changes>,
    ) -> Result<(), Stop> {
        self.set_pushing(true);
        let result = self.full_push(connection, pending).await;
        self.set_pushing(false);
        result
    }

    /// The steps of [`Self::push_full`], which the CLI is busy with as a whole: the listing and
    /// the hashing of a large tree take as long as sending it.
    async fn full_push(
        &mut self,
        connection: &mut PreviewSyncConnection<S>,
        pending: Option<&mut Changes>,
    ) -> Result<(), Stop> {
        let entries = self.entries.clone();
        let sources = self.sources.clone();
        let listed = match self
            .pack_blocking(move |watcher| list_entries(&entries, sources, watcher))
            .await?
        {
            Ok(listed) => listed,
            Err(error) => return self.failed_to_read_trees(error, pending),
        };

        self.sources = Some(listed.sources);
        self.rules = listed.rules.into();
        self.rule_files = listed.rule_files;

        // The push only carries the paths that the sidecar wants after it compared the
        // manifests with the volume, so a push after a pairing or a rule change sends little
        // when the volume already holds most of the tree.
        let push_id = self.take_push_id();
        let wanted = connection
            .exchange_manifest(push_id, listed.manifests, |event| self.observe(&event))
            .await;
        let mut wanted = match wanted {
            Ok(wanted) => wanted,
            Err(PreviewSyncError::Unpaired(reason)) if reason != UnpairReason::Replaced => {
                self.unpaired(reason);
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };

        let entries = self.entries.clone();
        let listings = listed.listings;
        let packed = self
            .pack_blocking(move |_| {
                listings
                    .iter()
                    .zip(entries.iter())
                    .enumerate()
                    .map(|(index, (listing, entry))| {
                        let wanted = wanted.remove(&(index as u32)).unwrap_or_default();
                        listing
                            .pack_wanted(index as u32, &entry.local, &wanted)
                            .map_err(|error| CliError::PreviewSyncReadFailed {
                                path: entry.local.clone(),
                                error,
                            })
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .await?;
        let packed = match packed {
            Ok(packed) => packed,
            Err(error) => return self.failed_to_read_trees(error, pending),
        };

        match self
            .send_push(connection, push_id, PushMode::Full, packed)
            .await?
        {
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
            // A full push builds on no generation.
            Pushed::StaleBase => Err(PreviewSyncError::Rejected(RejectReason::StaleBase).into()),
        }
    }

    /// What a full push does when it can't list or pack the trees, as [`Self::push_full`] says:
    /// stops the sync, or turns `pending` into a full push that the next change starts.
    fn failed_to_read_trees(
        &mut self,
        error: CliError,
        pending: Option<&mut Changes>,
    ) -> Result<(), Stop> {
        match pending {
            Some(pending) => {
                self.progress.warning(&with_causes(&error));
                pending.full = true;
                Ok(())
            }
            None => Err(Stop::Transient(Box::new(error))),
        }
    }

    /// Pushes `changes`: incrementally when the rules still hold and the volume holds this
    /// CLI's last push, and with a full push otherwise. When the changes can't be packed, such as
    /// a file that a tool removes while the CLI reads it, they turn `pending` into a full push,
    /// which the next change starts.
    async fn push_changes(
        &mut self,
        connection: &mut PreviewSyncConnection<S>,
        changes: Changes,
        pending: &mut Changes,
    ) -> Result<(), Stop> {
        self.set_pushing(true);
        let result = self.changes_push(connection, changes, pending).await;
        self.set_pushing(false);
        result
    }

    /// The steps of [`Self::push_changes`], which the CLI is busy with as a whole.
    async fn changes_push(
        &mut self,
        connection: &mut PreviewSyncConnection<S>,
        changes: Changes,
        pending: &mut Changes,
    ) -> Result<(), Stop> {
        let Some(base_generation) = self.generation.filter(|_| changes.full.not()) else {
            return self.full_push(connection, Some(pending)).await;
        };

        let entries = self.entries.clone();
        let rules = self.rules.clone();
        let packed = self
            .pack_blocking(move |watcher| pack_all_changes(&entries, &rules, &changes, watcher))
            .await?;

        let packed = match packed {
            Ok(Some(packed)) => packed,
            // A change touched the rules.
            Ok(None) => return self.full_push(connection, Some(pending)).await,
            Err(error) => {
                self.progress.warning(&with_causes(&error));
                pending.full = true;
                return Ok(());
            }
        };

        if packed.is_empty() {
            return Ok(());
        }

        let push_id = self.take_push_id();
        let mode = PushMode::Incremental { base_generation };
        match self.send_push(connection, push_id, mode, packed).await? {
            Pushed::Applied(_) => Ok(()),
            Pushed::StaleBase => self.full_push(connection, Some(pending)).await,
            Pushed::Unpaired(reason) => {
                self.unpaired(reason);
                Ok(())
            }
        }
    }

    /// Runs `pack` on a blocking thread with the watcher lent to it, so that it can start
    /// watching each directory right before it reads it.
    async fn pack_blocking<T: Send + 'static>(
        &mut self,
        pack: impl FnOnce(Option<&mut RecommendedWatcher>) -> T + Send + 'static,
    ) -> Result<T, Stop> {
        let mut watcher = self.watcher.as_mut().map(TreeWatcher::take);

        let (watcher, result) = tokio::task::spawn_blocking(move || {
            let result = pack(watcher.as_mut());
            (watcher, result)
        })
        .await
        .map_err(|error| Stop::Fatal(Box::new(CliError::PreviewSyncFailed(Box::new(error)))))?;

        if let (Some(tree_watcher), Some(watcher)) = (&mut self.watcher, watcher) {
            tree_watcher.put_back(watcher);
        }

        Ok(result)
    }

    /// Takes the id of the next push.
    fn take_push_id(&mut self) -> u64 {
        let push_id = self.next_push_id;
        self.next_push_id = self.next_push_id.wrapping_add(1);
        push_id
    }

    /// Whether the CLI is busy with a push, which keeps the creation timeout from running.
    fn set_pushing(&mut self, pushing: bool) {
        self.pushing = pushing;
        self.publish(false);
    }

    /// Sends `packed` as the push `push_id` in `mode`.
    async fn send_push(
        &mut self,
        connection: &mut PreviewSyncConnection<S>,
        push_id: u64,
        mode: PushMode,
        packed: Vec<PackedEntry>,
    ) -> Result<Pushed, Stop> {
        let result = connection
            .push(push_id, mode, packed, |event| self.observe(&event))
            .await;
        self.publish(result.is_ok());

        let generation = match result {
            Ok(generation) => generation,
            Err(PreviewSyncError::Rejected(RejectReason::StaleBase)) => {
                return Ok(Pushed::StaleBase);
            }
            Err(PreviewSyncError::Unpaired(reason)) if reason != UnpairReason::Replaced => {
                return Ok(Pushed::Unpaired(reason));
            }
            Err(error) => return Err(error.into()),
        };

        self.generation = Some(generation);
        self.applied_pushes += 1;
        self.progress
            .info(&format!("pushed the local code (generation {generation})"));

        Ok(Pushed::Applied(generation))
    }

    /// Waits for the next `Paired` after the operator unpaired the connection for `reason`.
    /// The supervisor's last report described a pod that may be gone, so it no longer counts,
    /// and neither does the first push of the pairing that ended.
    fn unpaired(&mut self, reason: UnpairReason) {
        self.pairing = match reason {
            UnpairReason::Idle => Pairing::Idle,
            UnpairReason::Paused => Pairing::Paused,
            _ => Pairing::Waiting,
        };
        self.supervisor = SupervisorReport::default();
        self.activity
            .send_modify(|activity| activity.first_push = None);
        self.publish(false);
        self.progress
            .info(&format!("waiting to push again: {reason}"));
    }

    /// Reports `event`, and records what it says about the preview pod's work.
    fn observe(&mut self, event: &SyncEvent) {
        let progressed = match event {
            SyncEvent::Sidecar(state) => {
                if self.sidecar.as_ref() == Some(state) {
                    return;
                }
                self.progress.info(&format!("sync sidecar: {state}"));
                self.sidecar = Some(state.clone());

                // The supervisor's last report described a pod that may be gone.
                if *state == SidecarState::Disconnected {
                    self.supervisor = SupervisorReport::default();
                }
                true
            }
            SyncEvent::Supervisor(report) => {
                if *report == self.supervisor {
                    return;
                }
                if report.install != self.supervisor.install
                    && let Some(message) =
                        describe_install(&report.install, report.install.error.as_deref())
                {
                    self.progress.info(&message);
                }
                if report.app.state != self.supervisor.app.state {
                    self.progress
                        .info(&format!("app: {}", describe_app(&report.app)));
                }

                // A failed restart only counts up the app's restarts.
                let progressed = report.install.state != self.supervisor.install.state
                    || report.app.state != self.supervisor.app.state
                    || report.handled != self.supervisor.handled;
                self.supervisor = report.clone();
                progressed
            }
            SyncEvent::Paired { .. } | SyncEvent::Unpaired(_) => return,
        };

        self.publish(progressed);
    }

    /// Publishes whether the preview pod is busy, and with `progressed` counts one more report
    /// of progress.
    fn publish(&self, progressed: bool) {
        let busy = self.pushing
            || self.supervisor.install.state == InstallState::Running
            || matches!(
                self.sidecar,
                Some(SidecarState::WaitingForVolume | SidecarState::Seeding)
            );

        self.activity.send_modify(|activity| {
            activity.busy = busy;
            activity.handled = self.supervisor.handled;
            activity.install_failed = self.supervisor.install.state == InstallState::Failed;
            activity.paused = self.pairing == Pairing::Paused;
            if progressed {
                activity.progress += 1;
            }
        });
    }
}

/// The listed entries of a full push, with what the push and the incremental pushes after it
/// need.
struct FullListing {
    /// The listing of each entry.
    listings: Vec<TreeListing>,

    /// The manifest of each entry's tree, with the entry's index.
    manifests: Vec<(u32, Vec<ManifestPath>)>,

    /// Where each entry's rules come from.
    sources: Arc<[RuleSources]>,

    /// The rules of each entry.
    rules: Vec<IgnoreRules>,

    /// The files outside the trees that the rules came from or could come from.
    rule_files: BTreeSet<PathBuf>,
}

/// Lists every entry and makes its manifest. Finds where each entry's rules come from, unless
/// `sources` holds them from an earlier listing. Starts watching each directory before it reads
/// it.
fn list_entries(
    entries: &[SyncEntry],
    sources: Option<Arc<[RuleSources]>>,
    mut watcher: Option<&mut RecommendedWatcher>,
) -> CliResult<FullListing> {
    let sources = match sources {
        Some(sources) => sources,
        None => entries
            .iter()
            .map(|entry| {
                RuleSources::new(&entry.local).map_err(|error| read_failed(&entry.local, error))
            })
            .collect::<Result<Vec<_>, _>>()?
            .into(),
    };
    let mut listings = Vec::with_capacity(entries.len());
    let mut manifests = Vec::with_capacity(entries.len());
    let mut rules = Vec::with_capacity(entries.len());
    let mut rule_files = BTreeSet::new();

    for (index, (entry, entry_sources)) in entries.iter().zip(sources.iter()).enumerate() {
        let failed = |error| read_failed(&entry.local, error);
        let watch_failed = |error| CliError::PreviewSyncWatchFailed {
            path: entry.local.clone(),
            error,
        };

        // A watch follows its directory, so only a watch of the parent reports a directory that
        // replaces the entry's own, such as one renamed into its place.
        if let (Some(watcher), Some(parent)) = (watcher.as_deref_mut(), entry.local.parent()) {
            watch_directory(watcher, parent).map_err(watch_failed)?;
        }

        let listing = TreeListing::new_watching(entry_sources, &entry.ignore, &mut |directory| {
            watcher.as_deref_mut().map_or(Ok(()), |watcher| {
                watch_directory(watcher, directory).map_err(io::Error::other)
            })
        })
        .map_err(failed)?;

        if let Some(watcher) = watcher.as_deref_mut() {
            // A change to a file that the rules come from, or could come from, changes the
            // rules, so its directory is watched too. Only the file's own events count (see
            // `Changes::merge`).
            for file in listing.rule_files() {
                if let Some(directory) = file.parent() {
                    watch_directory(watcher, directory).map_err(watch_failed)?;
                }
            }
        }

        manifests.push((
            index as u32,
            listing.manifest(&entry.local).map_err(failed)?,
        ));
        rules.push(listing.rules().clone());
        rule_files.extend(listing.rule_files().iter().cloned());
        listings.push(listing);
    }

    Ok(FullListing {
        listings,
        manifests,
        sources,
        rules,
        rule_files,
    })
}

/// Packs the changes of every entry in `changes` incrementally, with each entry's `rules`.
/// Returns `None` when a change touched the rules, so the trees need a full push. Leaves out the
/// entries without a change.
fn pack_all_changes(
    entries: &[SyncEntry],
    rules: &[IgnoreRules],
    changes: &Changes,
    mut watcher: Option<&mut RecommendedWatcher>,
) -> CliResult<Option<Vec<PackedEntry>>> {
    let mut packed = Vec::new();

    for (&index, changed) in &changes.per_entry {
        let (Some(entry), Some(rules)) = (entries.get(index), rules.get(index)) else {
            continue;
        };

        let packed_entry = pack_changes(
            index as u32,
            &entry.local,
            rules,
            changed,
            &mut |directory| {
                watcher.as_deref_mut().map_or(Ok(()), |watcher| {
                    watch_directory(watcher, directory).map_err(io::Error::other)
                })
            },
        )
        .map_err(|error| read_failed(&entry.local, error))?;

        let Some(packed_entry) = packed_entry else {
            return Ok(None);
        };

        if packed_entry.plan.files + packed_entry.plan.directories > 0
            || packed_entry.deleted.is_empty().not()
        {
            packed.push(packed_entry);
        }
    }

    Ok(Some(packed))
}

/// Starts watching `directory` alone, without its subdirectories, which the listing watches when
/// it reads them, so that an ignored tree such as `node_modules` takes no watch.
fn watch_directory(watcher: &mut RecommendedWatcher, directory: &Path) -> notify::Result<()> {
    match watcher.watch(directory, RecursiveMode::NonRecursive) {
        // A directory that is gone by now has its removal on the way as an event, and the
        // directory of a rule file that doesn't exist yet may not exist either.
        Err(error) if matches!(error.kind, notify::ErrorKind::PathNotFound) => Ok(()),
        result => result,
    }
}

/// The error of an entry whose tree under `path` the CLI couldn't read, or couldn't watch: a
/// listing that watches the directories it reads carries the watcher's error in an `io::Error`.
fn read_failed(path: &Path, error: io::Error) -> CliError {
    match error.downcast::<notify::Error>() {
        Ok(error) => CliError::PreviewSyncWatchFailed {
            path: path.to_owned(),
            error,
        },
        Err(error) => CliError::PreviewSyncReadFailed {
            path: path.to_owned(),
            error,
        },
    }
}

/// What the watcher saw change, by entry, since the last push.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Changes {
    /// The changed paths of each entry, by its index, relative to its `local`.
    per_entry: BTreeMap<usize, BTreeSet<PathBuf>>,

    /// Whether only a full push can sync the trees: the watcher lost events, the rules changed,
    /// or an earlier batch failed to pack.
    full: bool,
}

impl Changes {
    fn is_empty(&self) -> bool {
        self.full.not() && self.per_entry.is_empty()
    }

    /// Adds the paths of `batch` to every entry whose `local` holds them, leaving out the paths
    /// that the entry's `rules`, from its last full push, ignore whatever their kind. Local
    /// directories may overlap, and a file under two of them goes into two remotes. A change to
    /// a file in `rule_files`, outside the trees, or to a `.gitignore` that the rules read, in a
    /// directory that they don't ignore, changes the rules.
    fn merge(
        &mut self,
        batch: Batch,
        entries: &[SyncEntry],
        rule_files: &BTreeSet<PathBuf>,
        rules: &[IgnoreRules],
    ) {
        self.full |= batch.lost;

        for path in batch.paths {
            if rule_files.contains(&path) {
                self.full = true;
                continue;
            }

            for (index, entry) in entries.iter().enumerate() {
                let Ok(relative) = path.strip_prefix(&entry.local) else {
                    continue;
                };
                let entry_rules = rules.get(index);

                // A change to a child is the child's event, so an event of the entry's own
                // directory means that the directory was removed or replaced. Only a full push
                // lists the directory that is there now, and watches it.
                if relative.as_os_str().is_empty() {
                    self.full = true;
                    continue;
                }

                // A `.gitignore` in a directory that the rules don't ignore is a source of them,
                // which git reads also when a rule matches the file itself, so it changes the
                // rules whether it was created, changed or deleted. So does a directory that holds
                // a source, or lies above one, when it is replaced or deleted whole.
                if (relative
                    .file_name()
                    .is_some_and(|name| name == ".gitignore")
                    && relative.parent().is_none_or(|parent| {
                        entry_rules.is_none_or(|rules| rules.is_ignored(parent, true).not())
                    }))
                    || entry_rules.is_some_and(|rules| rules.has_source_within(relative))
                {
                    self.full = true;
                    continue;
                }

                if entry_rules.is_some_and(|rules| {
                    rules.is_ignored(relative, false) && rules.is_ignored(relative, true)
                }) {
                    continue;
                }

                self.per_entry
                    .entry(index)
                    .or_default()
                    .insert(relative.to_owned());
            }
        }
    }
}

/// The changes that the watcher saw since the last batch.
#[derive(Debug, Default)]
pub(super) struct Batch {
    /// The files, directories and symlinks that were created, changed, removed or renamed.
    paths: BTreeSet<PathBuf>,

    /// Whether the watcher lost events, so that only a new listing tells what changed.
    lost: bool,
}

/// What the watcher saw since the last batch. The watcher's handler collects it as the events
/// come, so that during a long push, while the CLI reads no events, the batch holds one entry per
/// changed path.
#[derive(Default)]
struct Collected {
    batch: Batch,

    /// Counts the events, so that a batch waits until they stop.
    events: u64,
}

/// Watches the directories of the pushed trees, one by one.
struct TreeWatcher {
    /// Lent to the listing while it runs on a blocking thread (see [`Self::take`]).
    watcher: Option<RecommendedWatcher>,

    collected: Arc<Mutex<Collected>>,

    /// Notified on each event.
    changed: Arc<Notify>,

    /// When the batch that [`Self::next_batch`] waits for must end, even if events keep coming.
    batch_ends_by: Option<tokio::time::Instant>,
}

impl TreeWatcher {
    fn new() -> CliResult<Self> {
        let collected = Arc::new(Mutex::new(Collected::default()));
        let changed = Arc::new(Notify::new());

        let watcher = notify::recommended_watcher({
            let collected = collected.clone();
            let changed = changed.clone();
            move |event: notify::Result<notify::Event>| {
                // The CLI's own reads of the tree are most of the events, so they don't take the
                // lock.
                if let Ok(event) = &event
                    && event.need_rescan().not()
                    && changes_the_tree(&event.kind).not()
                {
                    return;
                }

                let mut collected = collected.lock().expect("the lock holders don't panic");
                match event {
                    Ok(event) if event.need_rescan() => collected.batch.lost = true,
                    Ok(event) => collected.batch.paths.extend(event.paths),
                    Err(_) => collected.batch.lost = true,
                }
                collected.events += 1;
                drop(collected);
                changed.notify_one();
            }
        })
        .map_err(|error| CliError::PreviewSyncFailed(Box::new(error)))?;

        Ok(Self {
            watcher: Some(watcher),
            collected,
            changed,
            batch_ends_by: None,
        })
    }

    fn take(&mut self) -> RecommendedWatcher {
        self.watcher
            .take()
            .expect("the watcher is back before the next listing")
    }

    fn put_back(&mut self, watcher: RecommendedWatcher) {
        self.watcher = Some(watcher);
    }

    fn events(&self) -> u64 {
        self.collected
            .lock()
            .expect("the lock holders don't panic")
            .events
    }

    /// Waits for the next change, then until the files stay unchanged, so that one save of
    /// several files makes one push, and returns what changed.
    ///
    /// Cancel-safe: a call that is dropped before it returns leaves what it waited for to the
    /// next call.
    async fn next_batch(&mut self) -> Batch {
        let ends_by = match self.batch_ends_by {
            Some(ends_by) => ends_by,
            None => {
                while self.events() == 0 {
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
                break;
            }
        }

        self.batch_ends_by = None;
        let mut collected = self.collected.lock().expect("the lock holders don't panic");
        collected.events = 0;
        std::mem::take(&mut collected.batch)
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
/// events, which [`Event::need_rescan`](notify::Event::need_rescan) reports too.
fn changes_the_tree(kind: &EventKind) -> bool {
    match kind {
        EventKind::Create(_) | EventKind::Remove(_) | EventKind::Other => true,
        EventKind::Modify(ModifyKind::Metadata(MetadataKind::AccessTime)) => false,
        EventKind::Modify(_) => true,
        EventKind::Access(_) | EventKind::Any => false,
    }
}

/// The install's state in the words of `mirrord preview sync` and `mirrord preview status`, or
/// `None` when the install didn't run. `error` is the part of the install's error to show, which
/// starts on a line of its own when it has more than one line.
pub(super) fn describe_install(install: &InstallReport, error: Option<&str>) -> Option<String> {
    match (install.state, error) {
        (InstallState::NotRun, _) => None,
        (InstallState::Failed, Some(error)) if error.contains('\n') => {
            Some(format!("install: failed:\n{error}"))
        }
        (InstallState::Failed, Some(error)) => Some(format!("install: failed: {error}")),
        (state, _) => Some(format!("install: {state}")),
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
