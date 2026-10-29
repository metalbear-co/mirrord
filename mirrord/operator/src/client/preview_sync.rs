//! The CLI's side of preview code sync: the `previews/{name}/sync` websocket and the archives the
//! CLI pushes through it. See [`crate::sync`] for the protocol.

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::{
    fs::{FileType, Permissions},
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

use crate::sync::{MAX_DATA_CHUNK_BYTES, SidecarState, SyncFrame};

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
    Rejected(String),

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

    /// Pushes `archives`, each a gzip-compressed tar stream from [`pack_directory`] with the index
    /// of the entry it replaces, and returns the generation that the volume holds afterwards.
    ///
    /// The connection must be paired, see [`Self::wait_until_paired`].
    pub async fn push(
        &mut self,
        push_id: u64,
        archives: &[(u32, Vec<u8>)],
    ) -> Result<u64, PreviewSyncError> {
        self.send(&SyncFrame::Begin {
            push_id,
            entries: archives.iter().map(|(index, _)| *index).collect(),
        })
        .await?;

        for (index, archive) in archives {
            for chunk in archive.chunks(MAX_DATA_CHUNK_BYTES) {
                self.send(&SyncFrame::Data {
                    index: *index,
                    chunk: chunk.to_vec(),
                })
                .await?;
            }
        }

        self.send(&SyncFrame::Commit { push_id }).await?;

        loop {
            match self.recv().await? {
                SyncFrame::Applied {
                    push_id: applied,
                    generation,
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

/// Walks the tree that a push of `local` carries, in the same order every time: everything under
/// `local` except `.git` and `.jj`, the paths that git ignores, and the sockets, FIFOs and
/// devices, which a push can't hold. Doesn't yield `local` itself.
///
/// Git's ignore rules come from the `.gitignore` files in the tree and in the directories above
/// `local` up to the root of its repository, from the repository's `.git/info/exclude`, and from
/// the developer's global excludes file (`core.excludesFile`, by default `~/.config/git/ignore`).
/// `local` is often a service's directory inside a larger repository whose root `.gitignore`
/// excludes `node_modules` or `.env`, so the `.gitignore` files above `local` count too. The
/// `.gitignore` files above the repository's root don't, as in git, and outside a repository
/// nothing is ignored, which is `ignore`'s default (`require_git`).
fn walk_tree(local: &Path) -> impl Iterator<Item = io::Result<ignore::DirEntry>> {
    ignore::WalkBuilder::new(local)
        .standard_filters(false)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .parents(true)
        .filter_entry(|entry| entry.file_name() != ".git" && entry.file_name() != ".jj")
        .sort_by_file_name(Ord::cmp)
        .build()
        .filter_map(|entry| match entry {
            Ok(entry) if entry.depth() == 0 => None,
            Ok(entry) => entry
                .file_type()
                .is_some_and(|file_type| {
                    file_type.is_file() || file_type.is_dir() || file_type.is_symlink()
                })
                .then_some(Ok(entry)),
            Err(error) => Some(Err(io::Error::other(error))),
        })
}

/// Packs the tree under `local` into a gzip-compressed tar stream for [`PreviewSyncConnection::push`].
///
/// Packs what [`walk_tree`] yields. Symlinks are packed as symlinks.
pub fn pack_directory(local: &Path) -> io::Result<Vec<u8>> {
    let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
    builder.follow_symlinks(false);

    for entry in walk_tree(local) {
        let entry = entry?;
        let relative = entry
            .path()
            .strip_prefix(local)
            .expect("the walk only yields paths under its root");
        builder.append_path_with_name(entry.path(), relative)?;
    }

    builder.into_inner()?.finish()
}

/// One file, directory or symlink of a [`TreeListing`].
///
/// A directory has no size or times here: they change when an ignored file appears in it, and the
/// listing already has every child that the push carries.
#[derive(Debug, PartialEq, Eq)]
struct ListedPath {
    path: PathBuf,
    file_type: FileType,
    len: u64,
    modified: Option<SystemTime>,
    permissions: Permissions,

    /// On Unix, the inode and the time of the last status change. Every write updates the time,
    /// and no tool can set it back, unlike `modified`, which `cp -p` or `touch -r` restore.
    status_change: Option<(u64, i64, i64)>,
}

/// What [`pack_directory`] would pack from a tree, without the contents of the files. Two
/// listings of the same tree are equal, and a write to a packed file changes its listing, so a
/// listing that differs from the last push's means that the tree needs another push.
#[derive(Debug, PartialEq, Eq)]
pub struct TreeListing(Vec<ListedPath>);

impl TreeListing {
    /// Lists the tree under `local`.
    pub fn new(local: &Path) -> io::Result<Self> {
        walk_tree(local)
            .map(|entry| {
                let entry = entry?;
                let metadata = entry.metadata().map_err(io::Error::other)?;
                let is_dir = metadata.is_dir();

                #[cfg(unix)]
                let status_change = Some((metadata.ino(), metadata.ctime(), metadata.ctime_nsec()));
                #[cfg(not(unix))]
                let status_change = None;

                Ok(ListedPath {
                    path: entry.into_path(),
                    file_type: metadata.file_type(),
                    len: if is_dir { 0 } else { metadata.len() },
                    modified: metadata.modified().ok().filter(|_| is_dir.not()),
                    permissions: metadata.permissions(),
                    status_change: status_change.filter(|_| is_dir.not()),
                })
            })
            .collect::<io::Result<_>>()
            .map(Self)
    }
}
