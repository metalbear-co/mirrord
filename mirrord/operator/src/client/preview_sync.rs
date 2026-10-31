//! The CLI's side of preview code sync: the `previews/{name}/sync` websocket and the archives the
//! CLI pushes through it. See [`crate::sync`] for the protocol.

#[cfg(unix)]
use std::os::unix::{ffi::OsStrExt, fs::PermissionsExt};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, FileType, Permissions},
    io,
    ops::Not,
    path::{Path, PathBuf},
    time::Duration,
};

use flate2::{Compression, write::GzEncoder};
use futures::{SinkExt, StreamExt};
use hyper_util::rt::TokioIo;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{self, Message},
};

use crate::sync::{
    EntryPlan, MAX_DATA_CHUNK_BYTES, MAX_MANIFEST_CHUNK_BYTES, MAX_RULES_BYTES, ManifestKind,
    ManifestPath, PushMode, RejectReason, SidecarState, SupervisorReport, SyncFrame, UnpairReason,
    chunked,
    rules::{IgnoreRules, IgnoreSnapshot, IgnoreSource, join_relative},
    wanted_path_frame_cost,
};

#[cfg(test)]
mod tests;

/// Errors of a preview code sync.
#[derive(Debug, Error)]
pub enum PreviewSyncError {
    #[error("the sync connection to the operator failed: {0}")]
    WebSocket(#[from] tungstenite::Error),

    #[error("the operator closed the sync connection")]
    Closed,

    #[error("the operator sent a message that isn't a sync frame: {0}")]
    Decode(#[from] bincode::error::DecodeError),

    #[error("the sync stopped: {0}")]
    Operator(String),

    #[error("the preview pod rejected the push: {0}")]
    Rejected(RejectReason),

    #[error("the operator stopped the sync: {0}")]
    Unpaired(UnpairReason),

    #[error("the operator answered a push with an unexpected frame: {0:?}")]
    Unexpected(SyncFrame),

    #[error(
        "the ignore rules of the push take {0} bytes, more than the {MAX_RULES_BYTES} bytes that \
         one push can carry; the `.gitignore` files, the excludes files and the `ignore` lists of \
         the synced directories hold too many lines"
    )]
    RulesTooLarge(usize),
}

/// The HTTP connection to the operator after the upgrade to a websocket.
pub type UpgradedConnection = TokioIo<hyper::upgrade::Upgraded>;

/// The `previews/{name}/sync` websocket of one preview session, which carries one [`SyncFrame`]
/// per binary message.
///
/// `S` is the stream under the websocket, which tests replace.
pub struct PreviewSyncConnection<S = UpgradedConnection> {
    socket: WebSocketStream<S>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> PreviewSyncConnection<S> {
    pub fn new(socket: WebSocketStream<S>) -> Self {
        Self { socket }
    }

    /// Sends `frame`. When the send fails, the error says why the operator closed the
    /// connection if it did (see [`Self::why_sending_failed`]).
    async fn send(&mut self, frame: &SyncFrame) -> Result<(), PreviewSyncError> {
        match self
            .socket
            .send(Message::Binary(frame.encode().into()))
            .await
        {
            Ok(()) => Ok(()),
            Err(error) => Err(self.why_sending_failed(error.into()).await),
        }
    }

    /// The next frame from the operator. An [`SyncFrame::Error`] becomes
    /// [`PreviewSyncError::Operator`], because the operator closes the connection after it.
    async fn recv(&mut self) -> Result<SyncFrame, PreviewSyncError> {
        loop {
            let message = self.socket.next().await.ok_or(PreviewSyncError::Closed)??;
            let frame = match message {
                Message::Binary(bytes) => SyncFrame::decode(&bytes)?,
                Message::Close(_) => return Err(PreviewSyncError::Closed),
                _ => continue,
            };

            return match frame {
                SyncFrame::Error { message } => Err(PreviewSyncError::Operator(message)),
                frame => Ok(frame),
            };
        }
    }

    /// The next frame that the operator sends between pushes, as a [`SyncEvent`]. An answer to a
    /// push is unexpected here, because [`Self::push`] reads the answer to its own push.
    ///
    /// Cancel-safe: dropping the future before it returns loses no frame.
    pub async fn next_event(&mut self) -> Result<SyncEvent, PreviewSyncError> {
        let frame = self.recv().await?;
        SyncEvent::of(frame).map_err(PreviewSyncError::Unexpected)
    }

    /// Asks the operator to wake the preview if it is idle, as traffic to the preview would.
    pub async fn wake(&mut self) -> Result<(), PreviewSyncError> {
        self.send(&SyncFrame::WakeRequest).await
    }

    /// Sends the `manifests` of the full push `push_id`, each an entry's index and the manifest of
    /// its tree (see [`TreeListing::manifest`]), and returns the paths of each entry that the
    /// push must carry, as the sidecar answers. Calls `on_event` with the events that arrive
    /// meanwhile.
    ///
    /// The push itself follows with [`Self::push`], in [`PushMode::Full`] and with the same
    /// `push_id`, and carries only the returned paths (see [`TreeListing::pack_wanted`]). The
    /// sidecar keeps the manifest's other paths as the volume holds them, and deletes what the
    /// manifest lacks and the rules don't ignore.
    pub async fn exchange_manifest(
        &mut self,
        push_id: u64,
        manifests: Vec<(u32, Vec<ManifestPath>)>,
        mut on_event: impl FnMut(SyncEvent),
    ) -> Result<BTreeMap<u32, BTreeSet<Vec<u8>>>, PreviewSyncError> {
        for (index, manifest) in manifests {
            let mut chunks = chunked(manifest, ManifestPath::frame_cost);
            // An empty tree still sends its manifest, so that the sidecar knows the entry.
            if chunks.is_empty() {
                chunks.push(Vec::new());
            }

            for paths in chunks {
                self.send(&SyncFrame::Manifest {
                    push_id,
                    index,
                    paths,
                })
                .await?;
            }
        }

        self.send(&SyncFrame::ManifestDone { push_id }).await?;

        let mut wanted = BTreeMap::<u32, BTreeSet<Vec<u8>>>::new();
        loop {
            match self.recv().await? {
                SyncFrame::Want {
                    push_id: want,
                    index,
                    paths,
                } if want == push_id => wanted.entry(index).or_default().extend(paths),
                SyncFrame::WantDone { push_id: done } if done == push_id => return Ok(wanted),
                SyncFrame::Rejected {
                    push_id: rejected,
                    reason,
                } if rejected == push_id => return Err(PreviewSyncError::Rejected(reason)),
                SyncFrame::Unpaired { reason } => return Err(PreviewSyncError::Unpaired(reason)),
                SyncFrame::Status(state) => on_event(SyncEvent::Sidecar(state)),
                SyncFrame::Supervisor(report) => on_event(SyncEvent::Supervisor(report)),
                frame => return Err(PreviewSyncError::Unexpected(frame)),
            }
        }
    }

    /// Pushes `entries` in `mode`, and returns the generation that the volume holds afterwards.
    /// Calls `on_event` with the events that arrive while the sidecar applies the push.
    ///
    /// The connection must be paired, which [`SyncEvent::Paired`] reports. When the operator
    /// unpairs the connection before the sidecar answers, the push fails with
    /// [`PreviewSyncError::Unpaired`], and the CLI must push again once it is paired.
    pub async fn push(
        &mut self,
        push_id: u64,
        mode: PushMode,
        entries: Vec<PackedEntry>,
        mut on_event: impl FnMut(SyncEvent),
    ) -> Result<u64, PreviewSyncError> {
        self.send_push(push_id, mode, entries).await?;

        loop {
            match self.recv().await? {
                SyncFrame::Applied {
                    push_id: applied,
                    generation,
                    ..
                } if applied == push_id => return Ok(generation),
                SyncFrame::Rejected {
                    push_id: rejected,
                    reason,
                } if rejected == push_id => return Err(PreviewSyncError::Rejected(reason)),
                SyncFrame::Unpaired { reason } => return Err(PreviewSyncError::Unpaired(reason)),
                // The sidecar's state, and what the supervisor reports, can change while the
                // sidecar applies the push.
                SyncFrame::Status(state) => on_event(SyncEvent::Sidecar(state)),
                SyncFrame::Supervisor(report) => on_event(SyncEvent::Supervisor(report)),
                frame => return Err(PreviewSyncError::Unexpected(frame)),
            }
        }
    }

    /// Sends the frames of a push, which the operator reads only after the last one.
    async fn send_push(
        &mut self,
        push_id: u64,
        mode: PushMode,
        entries: Vec<PackedEntry>,
    ) -> Result<(), PreviewSyncError> {
        let (plans, streams): (Vec<_>, Vec<_>) = entries
            .into_iter()
            .map(|entry| {
                let index = entry.plan.index;
                (entry.plan, (index, entry.archive, entry.deleted))
            })
            .unzip();

        // The rules go in `Begin` whole, and a frame that the operator can't decode would only
        // close the connection, and fail each push again.
        let rules_cost = plans.iter().map(|plan| plan.rules.frame_cost()).sum();
        if rules_cost > MAX_RULES_BYTES {
            return Err(PreviewSyncError::RulesTooLarge(rules_cost));
        }

        self.send(&SyncFrame::Begin {
            push_id,
            mode,
            entries: plans,
        })
        .await?;

        for (index, archive, deleted) in streams {
            for chunk in archive.chunks(MAX_DATA_CHUNK_BYTES) {
                self.send(&SyncFrame::Data {
                    index,
                    chunk: chunk.to_vec(),
                })
                .await?;
            }

            self.send(&SyncFrame::EntryDone { index, deleted }).await?;
        }

        self.send(&SyncFrame::Commit { push_id }).await
    }

    /// Why sending a frame failed with `error`. The operator closes the connection after an
    /// `Unpaired` or an `Error`, so a large push or manifest can fail to send after the operator
    /// said why it stopped the sync, such as another CLI taking it over, which must not look like a
    /// broken connection. The frames that arrived before the connection closed say so.
    async fn why_sending_failed(&mut self, error: PreviewSyncError) -> PreviewSyncError {
        const DRAIN_TIMEOUT: Duration = Duration::from_secs(1);

        let reason = tokio::time::timeout(DRAIN_TIMEOUT, async {
            loop {
                match self.recv().await {
                    Ok(SyncFrame::Unpaired { reason }) => {
                        return Some(PreviewSyncError::Unpaired(reason));
                    }
                    Ok(_) => {}
                    Err(error @ PreviewSyncError::Operator(_)) => return Some(error),
                    Err(_) => return None,
                }
            }
        })
        .await;

        reason.ok().flatten().unwrap_or(error)
    }
}

/// What the operator tells the CLI between pushes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncEvent {
    /// The sidecar is ready for a push, and the volume holds `generation`, if any push reached
    /// it.
    Paired { generation: Option<u64> },

    /// The CLI can't push for now. The operator pairs it again later, except after
    /// [`UnpairReason::Replaced`].
    Unpaired(UnpairReason),

    /// The sidecar's state.
    Sidecar(SidecarState),

    /// What the preview pod's supervisor reports about the install and the app.
    Supervisor(SupervisorReport),
}

impl SyncEvent {
    /// The event that `frame` carries, or the frame itself when it isn't one.
    fn of(frame: SyncFrame) -> Result<Self, SyncFrame> {
        match frame {
            // The CLI speaks version 1, the oldest, so the sidecar's version changes nothing yet.
            SyncFrame::Paired { generation, .. } => Ok(Self::Paired { generation }),
            SyncFrame::Unpaired { reason } => Ok(Self::Unpaired(reason)),
            SyncFrame::Status(state) => Ok(Self::Sidecar(state)),
            SyncFrame::Supervisor(report) => Ok(Self::Supervisor(report)),
            frame => Err(frame),
        }
    }
}

/// One file, directory or symlink of a [`TreeListing`], with what its tar header needs.
#[derive(Debug)]
struct ListedPath {
    /// The path relative to the entry's local directory.
    path: PathBuf,
    file_type: FileType,
    permissions: Permissions,
}

/// Where the rules of the tree under a `local` directory come from: the repository that holds
/// it, and the excludes file of git's configuration. Only git's configuration moves them, so a
/// CLI that lists the tree again and again finds them once, and reads their contents on each
/// listing, which a developer may edit.
#[derive(Debug, Clone)]
pub struct RuleSources {
    local: PathBuf,

    /// The repository's root, the nearest directory above `local` with `.git` or `.jj`, and
    /// `local` relative to it. `None` outside a repository.
    repository: Option<(PathBuf, PathBuf)>,

    /// The excludes file of git's configuration and `.git/info/exclude`, which apply at the
    /// repository's root, whether or not they exist yet.
    exclude_files: Vec<PathBuf>,
}

impl RuleSources {
    /// Finds the sources of the rules of the tree under `local`. Asks `git` for its excludes
    /// file (see [`excludes_file`]), which is why a CLI should find them once per tree.
    ///
    /// The root is the nearest directory with `.git` or `.jj`, because jj honours the same
    /// `.gitignore` files, also in a repository that keeps its git data elsewhere.
    pub fn new(local: &Path) -> io::Result<Self> {
        let canonical = local.canonicalize()?;
        let Some(root) = canonical
            .ancestors()
            .find(|directory| directory.join(".git").exists() || directory.join(".jj").is_dir())
        else {
            return Ok(Self {
                local: local.to_owned(),
                repository: None,
                exclude_files: Vec::new(),
            });
        };

        let prefix = canonical
            .strip_prefix(root)
            .expect("the root is an ancestor of `local`")
            .to_owned();
        let exclude_files = [
            excludes_file(root),
            git_common_dir(root)?.map(|common| common.join("info").join("exclude")),
        ]
        .into_iter()
        .flatten()
        .collect();

        Ok(Self {
            local: local.to_owned(),
            repository: Some((root.to_owned(), prefix)),
            exclude_files,
        })
    }

    /// The rules that apply to the tree from its repository: the excludes files, and the
    /// `.gitignore` files in the directories above `local` up to the repository's root, with
    /// `local` relative to the root as the prefix. Also returns every file that these rules come
    /// from or could come from, including the ones that don't exist yet, because creating one
    /// changes the rules too. Outside a repository, there are none, and the prefix is empty.
    fn read(&self) -> io::Result<(IgnoreSnapshot, Vec<PathBuf>)> {
        let Some((root, prefix)) = &self.repository else {
            return Ok((IgnoreSnapshot::default(), Vec::new()));
        };
        let mut sources = Vec::new();
        let mut files = self.exclude_files.clone();

        for file in &self.exclude_files {
            if file.is_file() {
                sources.push(IgnoreSource {
                    root: Vec::new(),
                    lines: read_lines(file)?,
                });
            }
        }

        // The directories from the root down to the parent of `local`, whose `.gitignore` files
        // apply to `local`. The `.gitignore` of `local` itself is the first that the listing
        // reads.
        let mut directory = PathBuf::new();
        for component in prefix.components() {
            let gitignore = root.join(&directory).join(".gitignore");
            if fs::symlink_metadata(&gitignore).is_ok_and(|metadata| metadata.is_file()) {
                sources.push(IgnoreSource {
                    root: join_relative(b"", &directory),
                    lines: read_lines(&gitignore)?,
                });
            }
            files.push(gitignore);
            directory.push(component);
        }

        let snapshot = IgnoreSnapshot {
            prefix: join_relative(b"", prefix),
            sources,
        };
        Ok((snapshot, files))
    }
}

/// The tree that a push of one entry carries, without the contents of the files, and the rules
/// that left the other paths out.
///
/// The tree is everything under the entry's local directory that [`IgnoreRules`] doesn't ignore,
/// except sockets, FIFOs and devices, which a push can't hold, in the same order every time. The
/// rules come from the sources that git applies to the directory, listed in
/// [`Self::new_watching`].
#[derive(Debug)]
pub struct TreeListing {
    /// The rules that the listing applied, with every `.gitignore` of the tree added.
    rules: IgnoreRules,
    paths: Vec<ListedPath>,

    /// The files outside the tree that the snapshot's sources came from or could come from,
    /// such as the repository's root `.gitignore` or `.git/info/exclude`.
    rule_files: Vec<PathBuf>,
}

impl TreeListing {
    /// Lists the tree of `sources`, leaving out what git ignores and what `ignore`, the entry's
    /// own gitignore lines, matches. Calls `before_read` with each directory of the tree right
    /// before it reads the directory, so that a watcher that starts watching the directory there
    /// misses no change that the listing doesn't hold.
    ///
    /// The rules come from the sources that git reads, rooted where git roots them: the excludes
    /// file that git's configuration names (`core.excludesFile` of the repository's worktree
    /// and repository configuration, then of the developer's global configuration, by default
    /// `~/.config/git/ignore`) and the repository's `.git/info/exclude` at the repository's
    /// root, the `.gitignore` of every directory from the repository's root down to the tree,
    /// and the `.gitignore` of every directory in the tree that isn't ignored, as git reads them.
    /// `ignore` comes last, rooted at the tree. A tree is often a service's directory inside a
    /// larger repository whose root `.gitignore` excludes `node_modules` or `.env`, so the
    /// sources above it count too. The `.gitignore` files above the repository's root don't, as
    /// in git, and outside a repository only the `.gitignore` files in the tree and `ignore`
    /// apply.
    pub fn new_watching(
        sources: &RuleSources,
        ignore: &[String],
        before_read: &mut dyn FnMut(&Path) -> io::Result<()>,
    ) -> io::Result<Self> {
        let (snapshot, rule_files) = sources.read()?;
        let mut rules = IgnoreRules::new(snapshot);
        rules.add_last(Path::new(""), ignore.to_vec());
        let mut paths = Vec::new();
        visit(
            &sources.local,
            Path::new(""),
            &mut rules,
            &mut paths,
            before_read,
        )?;

        Ok(Self {
            rules,
            paths,
            rule_files,
        })
    }

    /// The rules that the listing applied, which an incremental push of the same tree applies
    /// too (see [`pack_changes`]).
    pub fn rules(&self) -> &IgnoreRules {
        &self.rules
    }

    /// The files outside the tree that the rules came from or could come from. A change to one
    /// of them, or to a `.gitignore` in the tree, changes the rules, so the tree needs a full
    /// push.
    pub fn rule_files(&self) -> &[PathBuf] {
        &self.rule_files
    }

    /// The manifest of the listed tree under `local`, for
    /// [`PreviewSyncConnection::exchange_manifest`]: every listed path with its kind, the mode,
    /// size and SHA-256 hash of each file, which it reads whole, and the target of each symlink.
    pub fn manifest(&self, local: &Path) -> io::Result<Vec<ManifestPath>> {
        self.paths
            .iter()
            .map(|listed| {
                let source = local.join(&listed.path);
                let kind = if listed.file_type.is_dir() {
                    ManifestKind::Dir
                } else if listed.file_type.is_symlink() {
                    let target = fs::read_link(&source).map_err(at(&source))?;
                    ManifestKind::Symlink {
                        target: target_bytes(&target),
                    }
                } else {
                    let mut file = File::open(&source).map_err(at(&source))?;
                    let mut hasher = Sha256::new();
                    let size = io::copy(&mut file, &mut hasher).map_err(at(&source))?;

                    ManifestKind::File {
                        mode: tar_mode(&listed.permissions, false) & 0o777,
                        size,
                        hash: hasher.finalize().into(),
                    }
                };

                Ok(ManifestPath {
                    path: join_relative(b"", &listed.path),
                    kind,
                })
            })
            .collect()
    }

    /// Packs the paths of the listed tree under `local` that `wanted` names, as
    /// [`SyncFrame::Want`] names them, into entry `index` of a full push after a manifest
    /// exchange. The sidecar keeps the other listed paths as the volume holds them.
    pub fn pack_wanted(
        &self,
        index: u32,
        local: &Path,
        wanted: &BTreeSet<Vec<u8>>,
    ) -> io::Result<PackedEntry> {
        let paths = self
            .paths
            .iter()
            .filter(|listed| wanted.contains(&join_relative(b"", &listed.path)));

        pack_paths(index, local, paths, self.rules.snapshot(), Vec::new())
    }
}

/// Packs the changes to the tree under `local` as entry `index` of an incremental push.
/// `changed` holds the paths that a watcher saw change, relative to `local`, and `rules` holds
/// the rules of the tree's last listing, which the push applies.
///
/// A changed path that exists goes into the stream, with everything below it if it is a
/// directory, which the sidecar then mirrors, so it loses the paths that the directory no
/// longer holds. A changed path that doesn't exist goes into the paths that the push deletes,
/// unless the rules ignore it whatever its kind was: the sidecar decides by the kind that the
/// path has on the volume. Ignored paths are left out of the stream. Calls `before_read` with
/// each directory that it reads, as [`TreeListing::new_watching`] does.
///
/// Returns `None` when a change touches the rules, such as a `.gitignore` that changed or that a
/// new directory holds, because the rules no longer hold then, so the tree needs a full push. So
/// does a change that deletes more paths than one [`SyncFrame::EntryDone`] can list, such as a
/// branch switch, because a full push deletes by its manifest, which it sends in chunks.
pub fn pack_changes(
    index: u32,
    local: &Path,
    rules: &IgnoreRules,
    changed: &BTreeSet<PathBuf>,
    before_read: &mut dyn FnMut(&Path) -> io::Result<()>,
) -> io::Result<Option<PackedEntry>> {
    let mut walk_rules = rules.clone();
    let mut paths = Vec::new();
    let mut deleted = Vec::new();
    // The directories that the stream holds whole, whose changed descendants it holds already.
    let mut packed_directories: Vec<&Path> = Vec::new();
    // The deleted paths, whose descendants are gone too, and which the sidecar deletes whole.
    let mut deleted_paths: Vec<&Path> = Vec::new();

    for path in changed {
        if packed_directories
            .iter()
            .chain(&deleted_paths)
            .any(|directory| path.starts_with(directory))
        {
            continue;
        }

        // A `.gitignore` in a directory that the rules don't ignore is a source of them, which git
        // reads also when a rule matches the file itself. So creating, changing or deleting one
        // changes the rules, and only a full push applies them to the whole tree. A directory that
        // holds a source, or lies above one, can also be replaced or deleted whole, which no event
        // of its `.gitignore` reports.
        if (path.file_name().is_some_and(|name| name == ".gitignore")
            && path
                .parent()
                .is_none_or(|parent| rules.is_ignored(parent, true).not()))
            || rules.has_source_within(path)
        {
            return Ok(None);
        }

        let metadata = match fs::symlink_metadata(local.join(path)) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if (rules.is_ignored(path, false) && rules.is_ignored(path, true)).not() {
                    deleted.push(join_relative(b"", path));
                    deleted_paths.push(path);
                }
                continue;
            }
            Err(error) => return Err(at(&local.join(path))(error)),
        };

        let file_type = metadata.file_type();
        let is_dir = file_type.is_dir();
        if (file_type.is_file() || is_dir || file_type.is_symlink()).not()
            || rules.is_ignored(path, is_dir)
        {
            continue;
        }

        paths.push(listed(path.clone(), &metadata));

        if is_dir {
            // The walk adds the `.gitignore` of every directory below `path` to the rules, also
            // one that the rules ignore, which git reads all the same.
            let sources = walk_rules.snapshot().sources.len();
            visit(local, path, &mut walk_rules, &mut paths, before_read)?;
            if walk_rules.snapshot().sources.len() != sources {
                return Ok(None);
            }
            packed_directories.push(path);
        }
    }

    let deleted_cost = deleted
        .iter()
        .map(|path| wanted_path_frame_cost(path))
        .sum::<usize>();
    if deleted_cost > MAX_MANIFEST_CHUNK_BYTES {
        return Ok(None);
    }

    pack_paths(index, local, &paths, rules.snapshot(), deleted).map(Some)
}

/// Packs `paths`, listed under `local`, into the gzip-compressed tar stream of entry `index`,
/// with the rules of `snapshot` and the paths that the push deletes. Symlinks are packed as
/// symlinks.
///
/// Reads each file whole before it writes its header, so the declared size is the size of what
/// the stream holds even when the file changes meanwhile.
fn pack_paths<'a>(
    index: u32,
    local: &Path,
    paths: impl IntoIterator<Item = &'a ListedPath>,
    snapshot: &IgnoreSnapshot,
    deleted: Vec<Vec<u8>>,
) -> io::Result<PackedEntry> {
    let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
    let mut files = 0;
    let mut directories = 0;
    let mut bytes = 0;

    for listed in paths {
        let source = local.join(&listed.path);
        let mut header = tar::Header::new_gnu();
        header.set_mode(tar_mode(&listed.permissions, listed.file_type.is_dir()));

        if listed.file_type.is_dir() {
            directories += 1;
            header.set_entry_type(tar::EntryType::Directory);
            header.set_size(0);
            builder.append_data(&mut header, &listed.path, io::empty())?;
        } else if listed.file_type.is_symlink() {
            files += 1;
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            let target = fs::read_link(&source).map_err(at(&source))?;
            builder.append_link(&mut header, &listed.path, target)?;
        } else {
            let contents = fs::read(&source).map_err(at(&source))?;
            files += 1;
            bytes += contents.len() as u64;
            header.set_size(contents.len() as u64);
            builder.append_data(&mut header, &listed.path, contents.as_slice())?;
        }
    }

    Ok(PackedEntry {
        plan: EntryPlan {
            index,
            files,
            directories,
            bytes,
            rules: snapshot.clone(),
        },
        archive: builder.into_inner()?.finish()?,
        deleted,
    })
}

/// The bytes of a symlink's `target` as it is written, which, unlike the paths of a tree, may be
/// absolute or leave the tree.
fn target_bytes(target: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        target.as_os_str().as_bytes().to_vec()
    }

    #[cfg(not(unix))]
    target.to_string_lossy().replace('\\', "/").into_bytes()
}

/// Lists the directory `relative` of the tree under `local` into `paths`, after it adds the
/// directory's `.gitignore` to `rules`, and then each subdirectory that the rules don't ignore.
/// Git reads the `.gitignore` of a directory before it decides on the directory's paths, and
/// never reads one in an ignored directory.
fn visit(
    local: &Path,
    relative: &Path,
    rules: &mut IgnoreRules,
    paths: &mut Vec<ListedPath>,
    before_read: &mut dyn FnMut(&Path) -> io::Result<()>,
) -> io::Result<()> {
    let directory = local.join(relative);
    before_read(&directory)?;

    let gitignore = directory.join(".gitignore");
    if fs::symlink_metadata(&gitignore).is_ok_and(|metadata| metadata.is_file()) {
        rules.add_gitignore(relative, read_lines(&gitignore)?);
    }

    let mut children = fs::read_dir(&directory)
        .and_then(|children| children.collect::<io::Result<Vec<_>>>())
        .map_err(at(&directory))?;
    children.sort_by_cached_key(|child| child.file_name());

    // Decides on every child before it enters a subdirectory, which adds the subdirectory's
    // `.gitignore` to the rules.
    let kept = {
        let directory_rules = rules.directory(relative);
        let mut kept = Vec::with_capacity(children.len());

        for child in children {
            let metadata = child.metadata().map_err(at(&child.path()))?;
            let file_type = metadata.file_type();
            let is_dir = file_type.is_dir();

            if (file_type.is_file() || is_dir || file_type.is_symlink())
                && directory_rules.is_ignored(&child.file_name(), is_dir).not()
            {
                kept.push((child.file_name(), metadata));
            }
        }

        kept
    };

    for (name, metadata) in kept {
        let path = relative.join(name);
        paths.push(listed(path.clone(), &metadata));

        if metadata.is_dir() {
            visit(local, &path, rules, paths, before_read)?;
        }
    }

    Ok(())
}

/// The listing of `path`, whose metadata is `metadata`.
fn listed(path: PathBuf, metadata: &fs::Metadata) -> ListedPath {
    ListedPath {
        path,
        file_type: metadata.file_type(),
        permissions: metadata.permissions(),
    }
}

/// The mode bits of a tar header for a path with `permissions`. The sidecar keeps the bits that
/// matter on Linux, such as the executable bits of a script.
fn tar_mode(permissions: &Permissions, is_dir: bool) -> u32 {
    #[cfg(unix)]
    {
        let _ = is_dir;
        permissions.mode() & 0o7777
    }

    #[cfg(not(unix))]
    match (is_dir, permissions.readonly()) {
        (true, _) => 0o755,
        (false, true) => 0o444,
        (false, false) => 0o644,
    }
}

/// One entry of a push: what [`SyncFrame::Begin`] declares for it, its gzip-compressed tar
/// stream, and, in an incremental push, the paths that it deletes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackedEntry {
    pub plan: EntryPlan,
    pub archive: Vec<u8>,

    /// The paths that an incremental push deletes from the entry, as
    /// [`SyncFrame::EntryDone`] lists them. A full push deletes none by name.
    pub deleted: Vec<Vec<u8>>,
}

/// The directory of the files that every worktree of the repository at `root`, the root of a
/// working tree, shares, such as `info/exclude`: `.git` itself, or, for a worktree, the main
/// repository's directory that its `commondir` names. `None` when the root has no `.git`, as in a
/// jj repository that keeps its git data elsewhere, or when its `.git` names no directory.
fn git_common_dir(root: &Path) -> io::Result<Option<PathBuf>> {
    let dot_git = root.join(".git");
    if !dot_git.exists() {
        return Ok(None);
    }
    if dot_git.is_dir() {
        return Ok(Some(dot_git));
    }

    let contents = fs::read_to_string(&dot_git)?;
    let Some(git_dir) = contents.trim().strip_prefix("gitdir:") else {
        return Ok(None);
    };
    let git_dir = root.join(git_dir.trim());

    match fs::read_to_string(git_dir.join("commondir")) {
        Ok(common) => Ok(Some(git_dir.join(common.trim()))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Some(git_dir)),
        Err(error) => Err(error),
    }
}

/// The excludes file that git uses for the repository at `root`, the root of the working tree.
///
/// Asks `git`, because only git itself reads every file of its configuration, including the
/// repository's own and the ones that `[include]` names. Without `git`, or when git refuses the
/// repository, only the global configuration's excludes file is known, which is
/// `~/.config/git/ignore` by default. A relative path is relative to `root`.
fn excludes_file(root: &Path) -> Option<PathBuf> {
    let git = mirrord_command::resolve_command("git")
        .arg("-C")
        .arg(root)
        .args(["config", "--path", "--get", "core.excludesFile"])
        .output();
    match git {
        Ok(output) if output.status.success() => {
            let value = String::from_utf8_lossy(&output.stdout);
            Some(root.join(value.trim_end_matches('\n')))
        }
        // Exit code 1 means that no configuration sets it, so git uses its default file.
        _ => ignore::gitignore::gitconfig_excludes_path(),
    }
}

/// The lines of the gitignore file at `path`. Git reads the file as bytes, so a line that isn't
/// UTF-8 keeps what it can.
fn read_lines(path: &Path) -> io::Result<Vec<String>> {
    let contents = fs::read(path).map_err(at(path))?;
    Ok(String::from_utf8_lossy(&contents)
        .lines()
        .map(str::to_owned)
        .collect())
}

/// Wraps an I/O error about `path` in an error of the same kind whose message names the path, so
/// that the error about one file of a large tree says which file it is. The CLI shows only the
/// message of an I/O error, so the message holds the cause too.
fn at(path: &Path) -> impl FnOnce(io::Error) -> io::Error {
    move |error| {
        io::Error::new(
            error.kind(),
            PathError {
                path: path.to_owned(),
                error,
            },
        )
    }
}

/// An I/O error about one file of a pushed tree.
#[derive(Debug, Error)]
#[error("`{}`: {error}", path.display())]
struct PathError {
    path: PathBuf,
    error: io::Error,
}
