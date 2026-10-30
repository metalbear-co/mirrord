//! The CLI's side of preview code sync: the `previews/{name}/sync` websocket and the archives the
//! CLI pushes through it. See [`crate::sync`] for the protocol.

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::{
    fs::{self, FileType, Permissions},
    io,
    ops::Not,
    path::{Path, PathBuf},
    time::SystemTime,
};

use flate2::{Compression, write::GzEncoder};
use futures::{SinkExt, StreamExt};
use hyper_util::rt::TokioIo;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{self, Message},
};

use crate::sync::{
    EntryPlan, MAX_DATA_CHUNK_BYTES, PushMode, RejectReason, SidecarState, SyncFrame,
    rules::{IgnoreRules, IgnoreSnapshot, IgnoreSource, join_relative},
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

    #[error("the operator answered a push with an unexpected frame: {0:?}")]
    Unexpected(SyncFrame),
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

    async fn send(&mut self, frame: &SyncFrame) -> Result<(), PreviewSyncError> {
        self.socket
            .send(Message::Binary(frame.encode().into()))
            .await?;
        Ok(())
    }

    /// The next frame from the operator. An [`SyncFrame::Error`] becomes
    /// [`PreviewSyncError::Operator`].
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

    /// Waits until the operator pairs this connection with the preview pod's sidecar, and calls
    /// `on_status` with each state that the sidecar reports meanwhile.
    ///
    /// Returns the generation that the volume holds, if any push reached it.
    pub async fn wait_until_paired(
        &mut self,
        mut on_status: impl FnMut(SidecarState),
    ) -> Result<Option<u64>, PreviewSyncError> {
        loop {
            match self.recv().await? {
                SyncFrame::Paired { generation } => return Ok(generation),
                SyncFrame::Status(state) => on_status(state),
                frame => return Err(PreviewSyncError::Unexpected(frame)),
            }
        }
    }

    /// Pushes `entries` as a full push, and returns the generation that the volume holds
    /// afterwards.
    ///
    /// The connection must be paired, see [`Self::wait_until_paired`].
    pub async fn push(
        &mut self,
        push_id: u64,
        entries: &[PackedEntry],
    ) -> Result<u64, PreviewSyncError> {
        self.send(&SyncFrame::Begin {
            push_id,
            mode: PushMode::Full,
            entries: entries.iter().map(|entry| entry.plan.clone()).collect(),
        })
        .await?;

        for entry in entries {
            let index = entry.plan.index;

            for chunk in entry.archive.chunks(MAX_DATA_CHUNK_BYTES) {
                self.send(&SyncFrame::Data {
                    index,
                    chunk: chunk.to_vec(),
                })
                .await?;
            }

            self.send(&SyncFrame::EntryDone {
                index,
                deleted: Vec::new(),
            })
            .await?;
        }

        self.send(&SyncFrame::Commit { push_id }).await?;

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
                // The sidecar's state can change while it applies the push.
                SyncFrame::Status(_) => continue,
                frame => return Err(PreviewSyncError::Unexpected(frame)),
            }
        }
    }
}

/// One file, directory or symlink of a [`TreeListing`].
///
/// A directory has no size or times here: they change when an ignored file appears in it, and the
/// listing already has every child that the push carries.
#[derive(Debug, PartialEq, Eq)]
struct ListedPath {
    /// The path relative to the entry's local directory.
    path: PathBuf,
    file_type: FileType,
    len: u64,
    modified: Option<SystemTime>,
    permissions: Permissions,

    /// On Unix, the inode and the time of the last status change. Every write updates the time,
    /// and no tool can set it back, unlike `modified`, which `cp -p` or `touch -r` restore.
    status_change: Option<(u64, i64, i64)>,
}

/// The tree that a push of one entry carries, without the contents of the files, and the rules
/// that left the other paths out.
///
/// The tree is everything under the entry's local directory that [`IgnoreRules`] doesn't ignore,
/// except sockets, FIFOs and devices, which a push can't hold, in the same order every time. The
/// rules come from the sources that git applies to the directory, listed in [`Self::new`].
///
/// Two listings of the same tree are equal, and a write to a pushed file or to a source of the
/// rules changes the listing, so a listing that differs from the last push's means that the tree
/// needs another push.
#[derive(Debug, PartialEq, Eq)]
pub struct TreeListing {
    snapshot: IgnoreSnapshot,
    paths: Vec<ListedPath>,
}

impl TreeListing {
    /// Lists the tree under `local`, leaving out what git ignores and what `ignore`, the entry's
    /// own gitignore lines, matches.
    ///
    /// The rules come from the sources that git reads, rooted where git roots them: the excludes
    /// file that git's configuration names (`core.excludesFile` of the repository's worktree
    /// and repository configuration, then of the developer's global configuration, by default
    /// `~/.config/git/ignore`) and the repository's `.git/info/exclude` at the repository's
    /// root, the `.gitignore` of every directory from the repository's root down to `local`, and
    /// the `.gitignore` of every directory in the tree that isn't ignored, as git reads them.
    /// `ignore` comes last, rooted at `local`. `local` is often a service's directory inside a
    /// larger repository whose root `.gitignore` excludes `node_modules` or `.env`, so the
    /// sources above `local` count too. The `.gitignore` files above the repository's root
    /// don't, as in git, and outside a repository only the `.gitignore` files in the tree and
    /// `ignore` apply.
    pub fn new(local: &Path, ignore: &[String]) -> io::Result<Self> {
        let mut rules = IgnoreRules::new(repository_rules(local)?);
        rules.add_last(Path::new(""), ignore.to_vec());
        let mut paths = Vec::new();
        visit(local, Path::new(""), &mut rules, &mut paths)?;

        Ok(Self {
            snapshot: rules.into_snapshot(),
            paths,
        })
    }

    /// Packs the listed tree under `local` into a gzip-compressed tar stream, as entry `index`
    /// of a push. Symlinks are packed as symlinks.
    ///
    /// Reads each file whole before it writes its header, so the declared size is the size of
    /// what the stream holds even when the file changes meanwhile.
    pub fn pack(&self, index: u32, local: &Path) -> io::Result<PackedEntry> {
        let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
        let mut files = 0;
        let mut directories = 0;
        let mut bytes = 0;

        for listed in &self.paths {
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
                rules: self.snapshot.clone(),
            },
            archive: builder.into_inner()?.finish()?,
        })
    }
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
) -> io::Result<()> {
    let directory = local.join(relative);

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
        let is_dir = metadata.is_dir();

        #[cfg(unix)]
        let status_change = Some((metadata.ino(), metadata.ctime(), metadata.ctime_nsec()));
        #[cfg(not(unix))]
        let status_change = None;

        paths.push(ListedPath {
            path: path.clone(),
            file_type: metadata.file_type(),
            len: if is_dir { 0 } else { metadata.len() },
            modified: metadata.modified().ok().filter(|_| is_dir.not()),
            permissions: metadata.permissions(),
            status_change: status_change.filter(|_| is_dir.not()),
        });

        if is_dir {
            visit(local, &path, rules, paths)?;
        }
    }

    Ok(())
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

/// One entry of a push: what [`SyncFrame::Begin`] declares for it, and its gzip-compressed tar
/// stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackedEntry {
    pub plan: EntryPlan,
    pub archive: Vec<u8>,
}

/// The rules that apply to `local` from its repository: the excludes file of git's
/// configuration, `.git/info/exclude`, and the `.gitignore` files in the directories above
/// `local` up to the repository's root, with `local` relative to the root as the prefix. Outside
/// a repository, there are none, and the prefix is empty.
///
/// The root is the nearest directory with `.git` or `.jj`, because jj honours the same
/// `.gitignore` files, also in a repository that keeps its git data elsewhere.
fn repository_rules(local: &Path) -> io::Result<IgnoreSnapshot> {
    let local = local.canonicalize()?;
    let Some(root) = local
        .ancestors()
        .find(|directory| directory.join(".git").exists() || directory.join(".jj").is_dir())
    else {
        return Ok(IgnoreSnapshot::default());
    };

    let prefix = local
        .strip_prefix(root)
        .expect("the root is an ancestor of `local`");
    let mut sources = Vec::new();

    let exclude_files = [
        excludes_file(root),
        git_common_dir(root)?.map(|common| common.join("info").join("exclude")),
    ];
    for file in exclude_files.into_iter().flatten() {
        if file.is_file() {
            sources.push(IgnoreSource {
                root: Vec::new(),
                lines: read_lines(&file)?,
            });
        }
    }

    // The directories from the root down to the parent of `local`, whose `.gitignore` files
    // apply to `local`. The `.gitignore` of `local` itself is the first that the listing reads.
    let mut directory = PathBuf::new();
    for component in prefix.components() {
        let gitignore = root.join(&directory).join(".gitignore");
        if fs::symlink_metadata(&gitignore).is_ok_and(|metadata| metadata.is_file()) {
            sources.push(IgnoreSource {
                root: join_relative(b"", &directory),
                lines: read_lines(&gitignore)?,
            });
        }
        directory.push(component);
    }

    Ok(IgnoreSnapshot {
        prefix: join_relative(b"", prefix),
        sources,
    })
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
