//! Copying of remote files into a local directory, before the user process starts.
//!
//! Applications that repeatedly read the same remote files (e.g. an HTTP server reading
//! `/etc/ssl` on every request) pay a round trip to the agent on every read. The paths listed in
//! `feature.fs.prefetch` are instead copied once, here, into a private temporary directory. The
//! layer then serves reads of those paths from that copy, without involving the agent at all.
//!
//! The copy mirrors the remote layout: remote `/etc/ssl/cert.pem` is stored at
//! `<root>/etc/ssl/cert.pem`. The layer can therefore map a requested path to its local copy by
//! path alone, and fall back to the remote when there is no local copy. That fallback is what
//! makes the error handling here safe: prefetching is only an optimization, so a path that could
//! not be copied is reported and skipped rather than failing the whole run.

use std::{
    collections::HashSet,
    fs::{self, File},
    io::{self, Write},
    ops::Not,
    path::{Path, PathBuf},
    time::Duration,
};
#[cfg(unix)]
use std::{fs::Permissions, os::unix::fs::PermissionsExt};

use mirrord_progress::Progress;
use mirrord_protocol::file::{
    CloseDirRequest, CloseFileRequest, FdOpenDirRequest, MetadataInternal, OpenFileRequest,
    OpenOptionsInternal, READDIR_BATCH_VERSION, ReadDirBatchRequest, ReadDirRequest,
    ReadFileRequest, XstatRequest,
};
use mirrord_protocol_api::client::{ClientError, MirrordClient, MirrordClientRetry};
use thiserror::Error;
use tracing::Level;
use uuid::Uuid;

/// Bits of [`MetadataInternal::mode`] that hold the file type (`S_IFMT`).
///
/// Spelled out here because the CLI does not depend on `libc`.
const FILE_TYPE_MASK: u32 = 0o170000;

/// File type bits of a directory (`S_IFDIR`).
const FILE_TYPE_DIRECTORY: u32 = 0o040000;

/// File type bits of a regular file (`S_IFREG`).
const FILE_TYPE_REGULAR: u32 = 0o100000;

/// Bits of [`MetadataInternal::mode`] that hold the permissions.
const PERMISSION_MASK: u32 = 0o7777;

/// How much of a file we ask for in a single [`ReadFileRequest`].
const READ_CHUNK_SIZE: u64 = 128 * 1024;

/// How many directory entries we ask for in a single [`ReadDirBatchRequest`].
const READDIR_BATCH_SIZE: usize = 128;

/// Appended to the name of a file while it is being filled.
const PARTIAL_SUFFIX: &str = ".mirrord-partial";

#[derive(Debug, Error)]
pub(crate) enum PrefetchError {
    #[error("failed to create local directory `{0}`: {1}")]
    CreateDir(PathBuf, io::Error),

    #[error("failed to write local file `{0}`: {1}")]
    WriteFile(PathBuf, io::Error),

    #[error("failed to set permissions of `{0}`: {1}")]
    SetPermissions(PathBuf, io::Error),

    #[error("agent request failed: {0}")]
    Request(#[from] ClientError),

    #[error("`{0}` is neither a regular file nor a directory")]
    UnsupportedFileType(PathBuf),
}

/// Copies `paths` and everything below them from the remote filesystem into a fresh local
/// directory, and returns that directory.
///
/// Only a failure to create the directory itself is fatal. Paths that could not be copied are
/// reported through `progress` and left out, to be read from the remote as usual.
#[tracing::instrument(level = Level::TRACE, skip_all, err)]
pub(crate) async fn prefetch_remote_paths<P: Progress>(
    client: &MirrordClient,
    paths: &[String],
    timeout: Duration,
    progress: &P,
) -> Result<PathBuf, PrefetchError> {
    let mut progress = progress.subtask("prefetching remote files");

    let root = std::env::temp_dir().join(format!("mirrord-prefetch-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).map_err(|error| PrefetchError::CreateDir(root.clone(), error))?;

    let mut downloader = Downloader {
        client,
        timeout,
        root: root.clone(),
        visited_directories: Default::default(),
        directory_modes: Default::default(),
        batched_readdir: READDIR_BATCH_VERSION.matches(client.protocol_version()),
    };

    let roots = independent_roots(paths);

    let mut prefetched = 0;
    for &path in &roots {
        match downloader.download_tree(path).await {
            Ok(()) => prefetched += 1,
            Err(error) => progress.warning(&format!(
                "failed to prefetch `{}`: {error}. It will be read from the remote instead.",
                path.display()
            )),
        }
    }

    // Directory permissions are applied only once the whole tree is in place, because a remote
    // directory that denies writes to its owner would otherwise stop us from filling its local
    // copy.
    for (path, mode) in downloader.directory_modes {
        if let Err(error) = set_permissions(&path, mode) {
            progress.warning(&format!("{error}"));
        }
    }

    progress.success(Some(&format!(
        "prefetched {prefetched}/{} path(s)",
        roots.len()
    )));

    Ok(root)
}

fn independent_roots(paths: &[String]) -> Vec<&Path> {
    let mut paths = paths.iter().map(Path::new).collect::<Vec<_>>();
    // Ordering by components puts a directory immediately before everything inside it, so each
    // path need only be checked against the last one kept.
    paths.sort_unstable();

    let mut roots = Vec::<&Path>::with_capacity(paths.len());
    for path in paths {
        if roots
            .last()
            .is_some_and(|root| path.starts_with(root))
            .not()
        {
            roots.push(path);
        }
    }

    roots
}

/// Deletes the copies made by [`prefetch_remote_paths`] once the session is over.
///
/// The CLI cannot do this itself: it `execve`s into the user's binary, so it is long gone by the
/// time the session ends, and its destructors never run. The internal proxy is the one process
/// whose lifetime is the session, so it holds this guard and does the cleaning up.
///
/// A session that is killed outright leaves the copies behind, as nothing gets to run.
pub(crate) struct PrefetchedFilesGuard(PathBuf);

impl PrefetchedFilesGuard {
    /// Takes ownership of the directory named by `MIRRORD_FS_PREFETCH_DIR`, when there is one.
    pub(crate) fn from_env() -> Option<Self> {
        std::env::var_os(MIRRORD_FS_PREFETCH_DIR).map(|directory| Self(directory.into()))
    }
}

impl Drop for PrefetchedFilesGuard {
    fn drop(&mut self) {
        match fs::remove_dir_all(&self.0) {
            Ok(()) => {
                tracing::debug!(directory = %self.0.display(), "Removed prefetched files")
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => tracing::warn!(
                directory = %self.0.display(),
                %error,
                "Failed to remove the directory holding prefetched files",
            ),
        }
    }
}

struct Downloader<'a> {
    client: &'a MirrordClient,
    timeout: Duration,
    /// Local directory the remote layout is mirrored into.
    root: PathBuf,
    /// Remote `(device_id, inode)` pairs of the directories copied
    /// for the configured path currently being handled.
    ///
    /// Symlinks are followed, so without this a symlink pointing back
    /// up its own tree would make us recurse forever.
    visited_directories: HashSet<(u64, u64)>,
    /// Permissions to apply to copied directories once the copy is complete.
    directory_modes: Vec<(PathBuf, u32)>,
    /// Whether the agent supports [`ReadDirBatchRequest`].
    batched_readdir: bool,
}

impl Downloader<'_> {
    /// Copies `path`, and everything below it when it is a directory.
    ///
    /// The copy of a configured path is all or nothing: the first failure gives up on the tree
    /// and takes whatever was copied of it so far. The layer decides what to serve by looking for
    /// a local copy, so anything left behind would be served for the rest of the session in place
    /// of the remote file - a half written file read as the whole of it, a directory whose
    /// listing failed read as empty, or one missing an entry read as complete.
    async fn download_tree(&mut self, path: &Path) -> Result<(), PrefetchError> {
        self.visited_directories.clear();

        let mut pending = vec![path.to_path_buf()];
        let mut is_root = true;
        let mut failure = None;

        while let Some(remote) = pending.pop() {
            match self.download_one(&remote).await {
                Ok(children) => pending.extend(children),
                Err(PrefetchError::UnsupportedFileType(path)) if is_root.not() => {
                    tracing::debug!(
                        path = %path.display(),
                        "Not prefetching a path that is neither a regular file nor a directory",
                    )
                }

                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }

            is_root = false;
        }

        match failure {
            Some(error) => {
                self.discard(path);

                Err(error)
            }
            None => Ok(()),
        }
    }

    /// Removes the copy of the tree rooted at `remote`, and forgets the permissions owed to it.
    fn discard(&mut self, remote: &Path) {
        let local = self.local_path(remote);

        self.directory_modes
            .retain(|(directory, _)| directory.starts_with(&local).not());

        let removed = if local.is_dir() {
            fs::remove_dir_all(&local)
        } else {
            fs::remove_file(&local)
        };

        match removed {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => tracing::error!(
                path = %local.display(),
                %error,
                "Failed to remove a partially copied path, which leaves it to be served in place \
                 of the remote one",
            ),
        }
    }

    /// Copies a single remote path, returning the children to visit when it is a directory.
    async fn download_one(&mut self, remote: &Path) -> Result<Vec<PathBuf>, PrefetchError> {
        let metadata = self.xstat(remote).await?;
        let local = self.local_path(remote);

        match metadata.mode & FILE_TYPE_MASK {
            FILE_TYPE_DIRECTORY => {
                if self
                    .visited_directories
                    .insert((metadata.device_id, metadata.inode))
                    .not()
                {
                    return Ok(Vec::new());
                }

                fs::create_dir_all(&local)
                    .map_err(|error| PrefetchError::CreateDir(local.clone(), error))?;
                self.directory_modes.push((local, metadata.mode));

                let entries = self.read_dir(remote).await?;

                Ok(entries
                    .into_iter()
                    .map(|entry| remote.join(entry))
                    .collect())
            }

            FILE_TYPE_REGULAR => {
                self.download_file(remote, &local, metadata.mode).await?;

                Ok(Vec::new())
            }

            _ => Err(PrefetchError::UnsupportedFileType(remote.to_path_buf())),
        }
    }

    async fn xstat(&self, remote: &Path) -> Result<MetadataInternal, PrefetchError> {
        let response = self
            .client
            .make_request_retry(
                XstatRequest {
                    path: Some(remote.to_path_buf()),
                    fd: None,
                    follow_symlink: true,
                },
                self.timeout,
            )
            .await?;

        Ok(response.metadata)
    }

    async fn open_remote(&self, remote: &Path) -> Result<u64, PrefetchError> {
        let response = self
            .client
            .make_request_retry(
                OpenFileRequest {
                    path: remote.to_path_buf(),
                    open_options: OpenOptionsInternal {
                        read: true,
                        ..Default::default()
                    },
                },
                self.timeout,
            )
            .await?;

        Ok(response.fd)
    }

    async fn download_file(
        &self,
        remote: &Path,
        local: &Path,
        mode: u32,
    ) -> Result<(), PrefetchError> {
        if let Some(parent) = local.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| PrefetchError::CreateDir(parent.to_path_buf(), error))?;
        }

        let mut partial = local.as_os_str().to_owned();
        partial.push(PARTIAL_SUFFIX);
        let partial = PathBuf::from(partial);

        let fd = self.open_remote(remote).await?;
        let result = self.copy_contents(fd, &partial).await;
        self.client
            .make_request_no_response(CloseFileRequest { fd })
            .await;

        if let Err(error) = result {
            let _ = fs::remove_file(&partial);

            return Err(error);
        }

        set_permissions(&partial, mode)?;

        fs::rename(&partial, local)
            .map_err(|error| PrefetchError::WriteFile(local.to_path_buf(), error))
    }

    async fn copy_contents(&self, fd: u64, local: &Path) -> Result<(), PrefetchError> {
        let mut file = File::create(local)
            .map_err(|error| PrefetchError::WriteFile(local.to_path_buf(), error))?;

        loop {
            let response = self
                .client
                .make_request_retry(
                    ReadFileRequest {
                        remote_fd: fd,
                        buffer_size: READ_CHUNK_SIZE,
                    },
                    self.timeout,
                )
                .await?;

            if response.read_amount == 0 {
                break Ok(());
            }

            file.write_all(response.bytes.as_ref())
                .map_err(|error| PrefetchError::WriteFile(local.to_path_buf(), error))?;
        }
    }

    /// Lists the names of the entries of the remote directory at `remote`.
    async fn read_dir(&self, remote: &Path) -> Result<Vec<String>, PrefetchError> {
        let fd = self.open_remote(remote).await?;

        let dir_fd = match self
            .client
            .make_request_retry(FdOpenDirRequest { remote_fd: fd }, self.timeout)
            .await
        {
            Ok(response) => response.fd,
            Err(error) => {
                self.client
                    .make_request_no_response(CloseFileRequest { fd })
                    .await;

                return Err(error.into());
            }
        };

        let entries = self.drain_dir(dir_fd).await;

        self.client
            .make_request_no_response(CloseDirRequest { remote_fd: dir_fd })
            .await;
        self.client
            .make_request_no_response(CloseFileRequest { fd })
            .await;

        entries
    }

    async fn drain_dir(&self, dir_fd: u64) -> Result<Vec<String>, PrefetchError> {
        let mut entries = Vec::new();

        // HACK ideally this would be handled by protocol-api
        if self.batched_readdir {
            loop {
                let response = self
                    .client
                    .make_request_retry(
                        ReadDirBatchRequest {
                            remote_fd: dir_fd,
                            amount: READDIR_BATCH_SIZE,
                        },
                        self.timeout,
                    )
                    .await?;

                let received = response.dir_entries.len();
                entries.extend(response.dir_entries.into_iter().map(|entry| entry.name));

                if received < READDIR_BATCH_SIZE {
                    break;
                }
            }
        } else {
            while let Some(entry) = self
                .client
                .make_request_retry(ReadDirRequest { remote_fd: dir_fd }, self.timeout)
                .await?
                .direntry
            {
                entries.push(entry.name);
            }
        }

        Ok(entries)
    }

    /// Where the copy of `remote` lives locally.
    fn local_path(&self, remote: &Path) -> PathBuf {
        self.root
            .join(remote.strip_prefix(Path::new("/")).unwrap_or(remote))
    }
}

/// Applies the remote file's permission bits to its local copy.
///
/// Only permissions are restored. [`MetadataInternal`]'s timestamps carry just the sub-second
/// component of the remote ones (they come from `st_atime_nsec` and friends), so there is no
/// meaningful time to restore from them.
#[cfg(unix)]
fn set_permissions(path: &Path, mode: u32) -> Result<(), PrefetchError> {
    fs::set_permissions(path, Permissions::from_mode(mode & PERMISSION_MASK))
        .map_err(|error| PrefetchError::SetPermissions(path.to_path_buf(), error))
}

/// Remote files are always on a unix filesystem, so their permission bits have nothing to map onto
/// here.
#[cfg(not(unix))]
fn set_permissions(_path: &Path, _mode: u32) -> Result<(), PrefetchError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    /// A path inside another is copied by it anyway, whichever order they are configured in.
    #[rstest]
    #[case(&["/etc", "/etc/ssl"], &["/etc"])]
    #[case(&["/etc/ssl", "/etc"], &["/etc"])]
    #[case(&["/etc/ssl/certs", "/etc", "/etc/ssl"], &["/etc"])]
    #[case(&["/etc", "/etc"], &["/etc"])]
    // Sharing the start of a name is not being inside it.
    #[case(&["/etc/ssl", "/etc/sslkeys"], &["/etc/ssl", "/etc/sslkeys"])]
    #[case(&["/etc", "/etcetera"], &["/etc", "/etcetera"])]
    // Unrelated paths are all kept.
    #[case(&["/var/log", "/etc/ssl"], &["/etc/ssl", "/var/log"])]
    #[case(&["/"], &["/"])]
    #[case(&["/", "/etc"], &["/"])]
    fn collapses_paths_inside_other_paths(#[case] paths: &[&str], #[case] expected: &[&str]) {
        let paths = paths
            .iter()
            .map(|path| (*path).to_owned())
            .collect::<Vec<_>>();
        let expected = expected.iter().map(Path::new).collect::<Vec<_>>();

        assert_eq!(independent_roots(&paths), expected);
    }
}
