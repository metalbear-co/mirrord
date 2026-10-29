//! Code sync for `mirrord preview sync`: pushes the local directories of a service's `sync` into
//! its preview pod, once after the session is created, and again on every change under `--watch`.

use std::{ops::Not, path::PathBuf, sync::Arc, time::Duration};

use mirrord_operator::{
    client::{
        NoClientCert, OperatorApi,
        preview_sync::{PreviewSyncConnection, TreeListing, UpgradedConnection, pack_directory},
    },
    sync::SidecarState,
};
use mirrord_progress::{Progress, ProgressTracker};
use mirrord_up::SyncEntry;
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

/// The sync connection of a preview session whose sidecar holds a push of the local directories,
/// which `--watch` uses to push them again.
///
/// `S` is the stream under the connection's websocket, which tests replace.
pub(super) struct PreviewSync<S = UpgradedConnection> {
    connection: PreviewSyncConnection<S>,

    /// The local directory of each entry, by its index in the session's `sync`.
    locals: Arc<[PathBuf]>,

    /// The listing of each entry's tree when the sidecar last received it.
    pushed: Vec<TreeListing>,

    next_push_id: u64,
}

impl PreviewSync {
    /// Opens the sync connection of the session `name` in `namespace`, waits until the operator
    /// pairs it with the preview pod, and pushes `entries`. Reports on `progress`, and finishes it.
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
                if last_state.replace(state) != Some(state) {
                    progress.info(&format!("sync sidecar: {}", describe(state)));
                }
            })
            .await?;

        let locals = entries.into_iter().map(|entry| entry.local).collect();
        let (preview_sync, generation) = PreviewSync::first_push(connection, locals).await?;
        progress.info(&format!("pushed the local code (generation {generation})"));

        Ok(preview_sync)
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> PreviewSync<S> {
    /// Pushes the directory of each entry in `locals` on the paired `connection`, and returns
    /// the generation that the volume holds afterwards.
    async fn first_push(
        connection: PreviewSyncConnection<S>,
        locals: Arc<[PathBuf]>,
    ) -> CliResult<(Self, u64)> {
        let (pushed, archives) = pack(locals.clone(), Vec::new()).await?;
        let mut preview_sync = Self {
            connection,
            locals,
            pushed,
            next_push_id: 1,
        };

        let generation = preview_sync.push(&archives).await?;

        Ok((preview_sync, generation))
    }

    /// Pushes `archives`, and returns the generation that the volume holds afterwards.
    async fn push(&mut self, archives: &[(u32, Vec<u8>)]) -> CliResult<u64> {
        let push_id = self.next_push_id;
        self.next_push_id += 1;
        Ok(self.connection.push(push_id, archives).await?)
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

        for local in self.locals.iter() {
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
            let (listings, archives) = match pack(self.locals.clone(), pushed).await {
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

            if archives.is_empty() {
                continue;
            }

            let generation = self.push(&archives).await?;
            progress.info(&format!("pushed the local code (generation {generation})"));
        }
    }
}

/// Lists the tree of every entry in `locals`, and packs the entries whose listing differs from
/// their listing in `pushed`, the listings of the last push. Returns all the listings, and the
/// archives by entry index. Before the first push `pushed` is empty, so every entry is packed.
///
/// Lists each tree before it packs it, so that a change made while it packs makes the next
/// listing differ and leads to another push.
async fn pack(
    locals: Arc<[PathBuf]>,
    pushed: Vec<TreeListing>,
) -> CliResult<(Vec<TreeListing>, Vec<(u32, Vec<u8>)>)> {
    tokio::task::spawn_blocking(move || {
        let mut listings = Vec::with_capacity(locals.len());
        let mut archives = Vec::new();

        for (index, local) in locals.iter().enumerate() {
            let failed = |error| CliError::PreviewSyncReadFailed {
                path: local.clone(),
                error,
            };

            let listing = TreeListing::new(local).map_err(failed)?;

            if pushed.get(index) != Some(&listing) {
                archives.push((index as u32, pack_directory(local).map_err(failed)?));
            }

            listings.push(listing);
        }

        Ok((listings, archives))
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

fn describe(state: SidecarState) -> String {
    match state {
        SidecarState::Disconnected => "waiting for the preview pod".to_owned(),
        SidecarState::AwaitingFirstPush => "waiting for the first push".to_owned(),
        SidecarState::Synced { generation } => format!("holds generation {generation}"),
    }
}
