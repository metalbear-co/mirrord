//! Code sync for `mirrord preview sync`: pushes the local directories of a service's `sync` into
//! its preview pod, once after the session is created, and again on every change under `--watch`.

use std::{
    ops::Not,
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use mirrord_config::target::Target;
use mirrord_operator::{
    client::{
        NoClientCert, OperatorApi,
        preview_sync::{PackedEntry, PreviewSyncConnection, TreeListing, UpgradedConnection},
    },
    crd::preview::{PreviewSyncConfig, PreviewSyncEntry, PreviewSyncInstall},
    sync::SidecarState,
};
use mirrord_progress::{Progress, ProgressTracker};
use mirrord_up::{InstallConfig, RunType, SubprocessCfg, SyncEntry};
use notify::{
    EventKind, RecursiveMode, Watcher,
    event::{MetadataKind, ModifyKind},
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::Notify,
};

use crate::error::{CliError, CliResult};

#[cfg(test)]
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

/// The sync connection of a preview session whose sidecar holds a push of the local directories,
/// which `--watch` uses to push them again.
///
/// `S` is the stream under the connection's websocket, which tests replace.
pub(super) struct PreviewSync<S = UpgradedConnection> {
    connection: PreviewSyncConnection<S>,

    /// The `sync` entries of the service, by their index in the session's `sync`.
    entries: Arc<[SyncEntry]>,

    /// The listing of each entry's tree when the sidecar last received it.
    pushed: Vec<TreeListing>,

    next_push_id: u64,
}

impl PreviewSync {
    /// Opens the sync connection of the session `name` in `namespace`, waits until the operator
    /// pairs it with the preview pod, and pushes `entries`, the service's `sync` entries in the
    /// order of the session's `spec.sync`. Reports on `progress`, and finishes it.
    pub(super) async fn start(
        operator_api: &OperatorApi<NoClientCert>,
        namespace: &str,
        name: &str,
        entries: Vec<SyncEntry>,
        mut progress: ProgressTracker,
    ) -> CliResult<Self> {
        let result =
            Self::connect_and_push(operator_api, namespace, name, entries, &progress).await;

        match &result {
            Ok(_) => progress.success(Some("pushed the local code")),
            Err(_) => progress.failure(None),
        }

        result
    }

    async fn connect_and_push(
        operator_api: &OperatorApi<NoClientCert>,
        namespace: &str,
        name: &str,
        entries: Vec<SyncEntry>,
        progress: &ProgressTracker,
    ) -> CliResult<Self> {
        let mut connection = operator_api.connect_preview_sync(namespace, name).await?;

        let mut last_state = None;
        connection
            .wait_until_paired(|state| {
                if last_state.as_ref() != Some(&state) {
                    progress.info(&format!("sync sidecar: {}", describe(&state)));
                    last_state = Some(state);
                }
            })
            .await?;

        let (preview_sync, generation) =
            PreviewSync::first_push(connection, entries.into()).await?;
        progress.info(&format!("pushed the local code (generation {generation})"));

        Ok(preview_sync)
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> PreviewSync<S> {
    /// Pushes the directory of each entry in `entries` on the paired `connection`, and returns
    /// the generation that the volume holds afterwards.
    async fn first_push(
        connection: PreviewSyncConnection<S>,
        entries: Arc<[SyncEntry]>,
    ) -> CliResult<(Self, u64)> {
        let (pushed, packed) = pack(entries.clone(), Vec::new()).await?;
        let mut preview_sync = Self {
            connection,
            entries,
            pushed,
            next_push_id: 1,
        };

        let generation = preview_sync.push(&packed).await?;

        Ok((preview_sync, generation))
    }

    /// Pushes `packed`, and returns the generation that the volume holds afterwards.
    async fn push(&mut self, packed: &[PackedEntry]) -> CliResult<u64> {
        let push_id = self.next_push_id;
        self.next_push_id += 1;
        Ok(self.connection.push(push_id, packed).await?)
    }

    /// Pushes the entries whose trees changed whenever a file under them changes, until the push
    /// fails or the user stops the CLI.
    pub(super) async fn watch(mut self, progress: &ProgressTracker) -> CliResult<()> {
        let changed = Arc::new(Notify::new());
        let mut watcher = notify::recommended_watcher({
            let changed = changed.clone();
            move |event: notify::Result<notify::Event>| {
                if event.is_ok_and(|event| changes_the_tree(&event.kind)) {
                    changed.notify_one();
                }
            }
        })
        .map_err(|error| CliError::PreviewSyncFailed(Box::new(error)))?;

        for local in self.entries.iter().map(|entry| &entry.local) {
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

        // The trees can have changed since the first push, before anything watched them, so the
        // first pass lists them without waiting for a change.
        changed.notify_one();

        loop {
            changed.notified().await;

            // Waits until the files stay unchanged, so that one save of several files makes one
            // push.
            let _ = tokio::time::timeout(WATCH_MAX_DELAY, async {
                while tokio::time::timeout(WATCH_DEBOUNCE, changed.notified())
                    .await
                    .is_ok()
                {}
            })
            .await;

            // A change under `.git` or under an ignored path, such as a build's output, leaves
            // the listings as they were, so it pushes nothing.
            let pushed = std::mem::take(&mut self.pushed);
            let (listings, packed) = match pack(self.entries.clone(), pushed).await {
                Ok(packed) => packed,
                // A file that an editor or a tool removes while the CLI reads the tree fails the
                // pack, and its removal is a change, so the next pass packs the trees again.
                // `pushed` stays empty until then, so that pass packs every entry.
                Err(error) => {
                    progress.warning(&with_causes(&error));
                    continue;
                }
            };
            self.pushed = listings;

            if packed.is_empty() {
                continue;
            }

            let generation = self.push(&packed).await?;
            progress.info(&format!("pushed the local code (generation {generation})"));
        }
    }
}

/// Lists the tree of every entry in `entries`, and packs the entries whose listing differs from
/// their listing in `pushed`, the listings of the last push. Returns all the listings, and the
/// packed entries. Before the first push `pushed` is empty, so every entry is packed.
///
/// Lists each tree before it packs it, so that a change made while it packs makes the next
/// listing differ and leads to another push.
async fn pack(
    entries: Arc<[SyncEntry]>,
    pushed: Vec<TreeListing>,
) -> CliResult<(Vec<TreeListing>, Vec<PackedEntry>)> {
    tokio::task::spawn_blocking(move || {
        let mut listings = Vec::with_capacity(entries.len());
        let mut packed = Vec::new();

        for (index, entry) in entries.iter().enumerate() {
            let failed = |error| CliError::PreviewSyncReadFailed {
                path: entry.local.clone(),
                error,
            };

            let listing = TreeListing::new(&entry.local, &entry.ignore).map_err(failed)?;

            if pushed.get(index) != Some(&listing) {
                packed.push(listing.pack(index as u32, &entry.local).map_err(failed)?);
            }

            listings.push(listing);
        }

        Ok((listings, packed))
    })
    .await
    .map_err(|error| CliError::PreviewSyncFailed(Box::new(error)))?
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
/// every listing start the next one. `Other` includes the watcher's notice that the kernel dropped
/// events, after which only a new listing tells what changed.
fn changes_the_tree(kind: &EventKind) -> bool {
    match kind {
        EventKind::Create(_) | EventKind::Remove(_) | EventKind::Other => true,
        EventKind::Modify(ModifyKind::Metadata(MetadataKind::AccessTime)) => false,
        EventKind::Modify(_) => true,
        EventKind::Access(_) | EventKind::Any => false,
    }
}

fn describe(state: &SidecarState) -> String {
    match state {
        SidecarState::Disconnected => "waiting for the preview pod".to_owned(),
        SidecarState::WaitingForVolume => {
            "waiting for the previous pod to release the code volume".to_owned()
        }
        SidecarState::Seeding => "copying the image's files to the code volume".to_owned(),
        SidecarState::AwaitingFirstPush => "waiting for the first push".to_owned(),
        SidecarState::Synced { generation } => format!("holds generation {generation}"),
        SidecarState::Failed { message } => format!("failed: {message}"),
    }
}
