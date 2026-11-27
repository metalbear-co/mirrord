use std::{
    self,
    collections::{HashMap, VecDeque, hash_map::Entry},
    fs::{File, Metadata, ReadDir},
    io::{self, SeekFrom, prelude::*},
    iter::{Enumerate, Peekable},
    ops::{Not, RangeInclusive},
    os::{
        fd::AsRawFd,
        unix::{ffi::OsStrExt, fs::MetadataExt, prelude::FileExt},
    },
    path::{Path, PathBuf},
    ptr,
};

use bytes::Bytes;
use faccess::{AccessMode, PathExt as _};
use libc::DT_DIR;
use mirrord_protocol::{FileRequest, FileResponse, RemoteResult, ResponseError, file::*};
use nix::{
    fcntl::{self, OFlag},
    sys::stat::{Mode, fstat},
    unistd::UnlinkatFlags,
};
use tracing::{Level, error, trace};

use crate::{
    error::AgentResult,
    metrics::OPEN_FD_COUNT,
    util::target_root::{Directory, TargetRoot, proc_fd_path},
};

/// The most bytes one read request reads, so that a client can't make the agent allocate as much
/// memory as it asks for. A larger request gets a short read, as `read(2)` and `pread(2)` may
/// return. The layer caps `read(2)` at the same size, but not `pread(2)`, nor the intproxy's
/// buffered reads of `feature.fs.readonly_file_buffer`.
const MAX_READ_SIZE: u64 = 1024 * 1024;

#[derive(Debug)]
pub enum RemoteFile {
    File(File),
    Directory(Directory),
}

/// Turns the options of an open request into `open(2)` flags, rejecting the same combinations
/// as [`std::fs::OpenOptions`].
fn open_flags(options: OpenOptionsInternal) -> io::Result<OFlag> {
    let OpenOptionsInternal {
        read,
        write,
        append,
        truncate,
        create,
        create_new,
    } = options;
    let invalid = || io::Error::from_raw_os_error(libc::EINVAL);

    let access = match (read, write, append) {
        (true, false, false) => OFlag::O_RDONLY,
        (false, true, false) => OFlag::O_WRONLY,
        (true, true, false) => OFlag::O_RDWR,
        (false, _, true) => OFlag::O_WRONLY | OFlag::O_APPEND,
        (true, _, true) => OFlag::O_RDWR | OFlag::O_APPEND,
        (false, false, false) => return Err(invalid()),
    };

    match (write, append) {
        (true, false) => {}
        (false, false) if truncate || create || create_new => return Err(invalid()),
        (false, false) => {}
        (_, true) if truncate && !create_new => return Err(invalid()),
        (_, true) => {}
    }

    let creation = match (create, truncate, create_new) {
        (false, false, false) => OFlag::empty(),
        (true, false, false) => OFlag::O_CREAT,
        (false, true, false) => OFlag::O_TRUNC,
        (true, true, false) => OFlag::O_CREAT | OFlag::O_TRUNC,
        (_, _, true) => OFlag::O_CREAT | OFlag::O_EXCL,
    };

    Ok(access | creation)
}

#[derive(Debug)]
pub(crate) struct GetDEnts64Stream {
    inner: std::fs::ReadDir,
    current_and_parent: VecDeque<io::Result<DirEntryInternal>>,
    current_index: usize,
}

impl GetDEnts64Stream {
    fn new(inner: ReadDir, current_and_parent: VecDeque<io::Result<DirEntryInternal>>) -> Self {
        Self {
            inner,
            current_and_parent,
            current_index: 0,
        }
    }
}

impl Iterator for GetDEnts64Stream {
    type Item = io::Result<DirEntryInternal>;

    fn next(&mut self) -> Option<Self::Item> {
        // first send current and parent entries
        if let Some(entry) = self.current_and_parent.pop_front() {
            self.current_index += 1;
            return Some(entry);
        }

        let ret = self
            .inner
            .next()
            .map(|i| (self.current_index, i).try_into()) // Convert into DirEntryInternal.
            .transpose()
            .inspect_err(|err| error!("Converting DirEntry failed with {err:?}"))
            .transpose();
        self.current_index += 1;
        ret
    }
}

/// Serves the [`FileRequest`]s of one client, inside the filesystem of its target, or of the
/// agent itself when there's no target.
///
/// Requests are untrusted: a client may send any path or descriptor ID, not only the ones its
/// layer would. Only [`TargetRoot`] resolves the paths of requests, which keeps every file this
/// touches inside the target's root (see [`crate::util::target_root`]). The other operations act on
/// descriptors it returned, so a descriptor ID only reaches what the same client opened.
#[derive(Debug)]
pub(crate) struct FileManager {
    root: TargetRoot,
    open_files: HashMap<u64, RemoteFile>,
    dir_streams: HashMap<u64, Enumerate<ReadDir>>,
    getdents_streams: HashMap<u64, Peekable<GetDEnts64Stream>>,
    fds_iter: RangeInclusive<u64>,
}

impl Drop for FileManager {
    fn drop(&mut self) {
        let descriptors =
            self.open_files.len() + self.dir_streams.len() + self.getdents_streams.len();
        OPEN_FD_COUNT.fetch_sub(descriptors, std::sync::atomic::Ordering::Relaxed);
    }
}

impl FileManager {
    /// Executes the request and returns the response.
    #[tracing::instrument(level = Level::TRACE, skip(self), ret, err(level = Level::DEBUG))]
    pub(crate) fn handle_message(
        &mut self,
        request: FileRequest,
    ) -> AgentResult<Option<FileResponse>> {
        Ok(match request {
            FileRequest::Open(OpenFileRequest { path, open_options }) => {
                Some(FileResponse::Open(self.open(None, path, open_options)))
            }
            FileRequest::OpenRelative(OpenRelativeFileRequest {
                relative_fd,
                path,
                open_options,
            }) => {
                let open_result = self.open(Some(relative_fd), path, open_options);
                Some(FileResponse::Open(open_result))
            }
            FileRequest::Read(ReadFileRequest {
                remote_fd,
                buffer_size,
            }) => {
                let read_result = self.read(remote_fd, buffer_size);
                Some(FileResponse::Read(read_result))
            }
            FileRequest::ReadLimited(ReadLimitedFileRequest {
                remote_fd,
                buffer_size,
                start_from,
            }) => Some(FileResponse::ReadLimited(self.read_limited(
                remote_fd,
                buffer_size,
                start_from,
            ))),
            FileRequest::ReadLink(ReadLinkFileRequest { path }) => {
                Some(FileResponse::ReadLink(self.read_link(path)))
            }
            FileRequest::Seek(SeekFileRequest { fd, seek_from }) => {
                let seek_result = self.seek(fd, seek_from.into());
                Some(FileResponse::Seek(seek_result))
            }
            FileRequest::Write(WriteFileRequest { fd, write_bytes }) => {
                let write_result = self.write(fd, write_bytes.0);
                Some(FileResponse::Write(write_result))
            }
            FileRequest::WriteLimited(WriteLimitedFileRequest {
                remote_fd,
                start_from,
                write_bytes,
            }) => {
                let write_result = self.write_limited(remote_fd, start_from, write_bytes.0);
                Some(FileResponse::WriteLimited(write_result))
            }
            FileRequest::Close(CloseFileRequest { fd }) => self.close(fd),
            FileRequest::Access(AccessFileRequest { pathname, mode }) => {
                Some(FileResponse::Access(self.access(pathname, mode)))
            }
            FileRequest::Xstat(XstatRequest {
                path,
                fd,
                follow_symlink,
            }) => {
                let xstat_result = self.xstat(path, fd, follow_symlink);
                Some(FileResponse::Xstat(xstat_result))
            }
            FileRequest::XstatFs(XstatFsRequest { fd }) => {
                let xstatfs_result = self.fstatfs(fd);
                // convert V2 response to old response for old client.
                Some(FileResponse::XstatFs(xstatfs_result.map(Into::into)))
            }
            FileRequest::XstatFsV2(XstatFsRequestV2 { fd }) => {
                let xstatfs_result = self.fstatfs(fd);
                Some(FileResponse::XstatFsV2(xstatfs_result))
            }
            FileRequest::StatFs(StatFsRequest { path }) => {
                let statfs_result = self.statfs(path);
                // convert V2 response to old response for old client.
                Some(FileResponse::XstatFs(statfs_result.map(Into::into)))
            }
            FileRequest::StatFsV2(StatFsRequestV2 { path }) => {
                let statfs_result = self.statfs(path);
                Some(FileResponse::XstatFsV2(statfs_result))
            }
            FileRequest::Rename(RenameRequest { old_path, new_path }) => {
                let rename_result = self.rename(old_path, new_path);
                Some(FileResponse::Rename(rename_result))
            }

            // dir operations
            FileRequest::FdOpenDir(FdOpenDirRequest { remote_fd }) => {
                let open_dir_result = self.fdopen_dir(remote_fd);
                Some(FileResponse::OpenDir(open_dir_result))
            }
            FileRequest::ReadDir(ReadDirRequest { remote_fd }) => {
                let read_dir_result = self.read_dir(remote_fd);
                Some(FileResponse::ReadDir(read_dir_result))
            }
            FileRequest::ReadDirBatch(ReadDirBatchRequest { remote_fd, amount }) => {
                let read_dir_result = self.read_dir_batch(remote_fd, amount);
                Some(FileResponse::ReadDirBatch(read_dir_result))
            }
            FileRequest::CloseDir(CloseDirRequest { remote_fd }) => self.close_dir(remote_fd),
            FileRequest::GetDEnts64(GetDEnts64Request {
                remote_fd,
                buffer_size,
            }) => Some(FileResponse::GetDEnts64(
                self.getdents64(remote_fd, buffer_size),
            )),
            FileRequest::MakeDir(MakeDirRequest { pathname, mode }) => {
                Some(FileResponse::MakeDir(self.mkdir(None, &pathname, mode)))
            }
            FileRequest::MakeDirAt(MakeDirAtRequest {
                dirfd,
                pathname,
                mode,
            }) => Some(FileResponse::MakeDir(self.mkdir(
                Some(dirfd),
                &pathname,
                mode,
            ))),
            FileRequest::RemoveDir(RemoveDirRequest { pathname }) => Some(FileResponse::RemoveDir(
                self.unlink(None, &pathname, libc::AT_REMOVEDIR as u32),
            )),
            FileRequest::Unlink(UnlinkRequest { pathname }) => {
                Some(FileResponse::Unlink(self.unlink(None, &pathname, 0)))
            }
            FileRequest::UnlinkAt(UnlinkAtRequest {
                dirfd,
                pathname,
                flags,
            }) => Some(FileResponse::Unlink(self.unlink(dirfd, &pathname, flags))),
            FileRequest::Ftruncate(FtruncateRequest { fd, length }) => {
                Some(FileResponse::Ftruncate(self.ftruncate(fd, length)))
            }
            FileRequest::Futimens(FutimensRequest { fd, times }) => {
                Some(FileResponse::Futimens(self.futimens(fd, times)))
            }
            FileRequest::Fchown(FchownRequest { fd, owner, group }) => {
                Some(FileResponse::Fchown(self.fchown(fd, owner, group)))
            }
            FileRequest::Fchmod(FchmodRequest { fd, mode }) => {
                Some(FileResponse::Fchmod(self.fchmod(fd, mode)))
            }
        })
    }

    /// Confines the requests to `root`.
    #[tracing::instrument(level = Level::TRACE, ret)]
    pub fn new(root: TargetRoot) -> Self {
        Self {
            root,
            open_files: Default::default(),
            dir_streams: Default::default(),
            getdents_streams: Default::default(),
            fds_iter: (0..=u64::MAX),
        }
    }

    /// Looks up the directory that a request's relative `path` resolves from.
    ///
    /// Returns [`None`] for an absolute `path` or no `dirfd`, which resolve from the target's
    /// root, the way `*at(2)` syscalls ignore their `dirfd` for an absolute path.
    fn base<'a>(
        open_files: &'a HashMap<u64, RemoteFile>,
        dirfd: Option<u64>,
        path: &Path,
    ) -> RemoteResult<Option<&'a Directory>> {
        match dirfd {
            Some(dirfd) if path.is_relative() => match open_files.get(&dirfd) {
                Some(RemoteFile::Directory(directory)) => Ok(Some(directory)),
                Some(RemoteFile::File(_)) => Err(ResponseError::NotDirectory(dirfd)),
                None => Err(ResponseError::NotFound(dirfd)),
            },
            _ => Ok(None),
        }
    }

    /// Handles both `open` and `openat`, which has a `dirfd`.
    #[tracing::instrument(level = Level::TRACE, skip(self), ret, err(level = Level::DEBUG))]
    fn open(
        &mut self,
        dirfd: Option<u64>,
        path: PathBuf,
        open_options: OpenOptionsInternal,
    ) -> RemoteResult<OpenFileResponse> {
        let base = Self::base(&self.open_files, dirfd, &path)?;
        let file = File::from(self.root.open(
            base,
            &path,
            open_flags(open_options)?,
            Mode::from_bits_truncate(0o666),
        )?);

        let remote_file = if file.metadata()?.is_dir() {
            RemoteFile::Directory(Directory { file })
        } else {
            RemoteFile::File(file)
        };

        let fd = self
            .fds_iter
            .next()
            .ok_or_else(|| ResponseError::IdsExhausted("open".to_owned()))?;

        if self.open_files.insert(fd, remote_file).is_none() {
            OPEN_FD_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }

        Ok(OpenFileResponse { fd })
    }

    #[tracing::instrument(level = "trace", skip(self))]
    pub(crate) fn read(&mut self, fd: u64, buffer_size: u64) -> RemoteResult<ReadFileResponse> {
        self.open_files
            .get_mut(&fd)
            .ok_or(ResponseError::NotFound(fd))
            .and_then(|remote_file| match remote_file {
                RemoteFile::File(file) => Ok(file),
                RemoteFile::Directory(_) => Err(ResponseError::NotFile(fd)),
            })
            .and_then(|file| {
                let mut buffer = Vec::with_capacity(4096);
                let read_amount = file
                    .take(buffer_size.min(MAX_READ_SIZE))
                    .read_to_end(&mut buffer)?;

                // Create the response with the read bytes and the read amount
                let response = ReadFileResponse {
                    bytes: buffer.into(),
                    read_amount: read_amount as u64,
                };

                Ok(response)
            })
    }

    #[tracing::instrument(level = "trace", skip(self))]
    pub(crate) fn read_limited(
        &mut self,
        fd: u64,
        buffer_size: u64,
        start_from: u64,
    ) -> RemoteResult<ReadFileResponse> {
        self.open_files
            .get_mut(&fd)
            .ok_or(ResponseError::NotFound(fd))
            .and_then(|remote_file| match remote_file {
                RemoteFile::File(file) => Ok(file),
                RemoteFile::Directory(_) => Err(ResponseError::NotFile(fd)),
            })
            .and_then(|file| {
                let mut buffer = vec![0; buffer_size.min(MAX_READ_SIZE) as usize];
                let read_amount = file.read_at(&mut buffer, start_from)?;

                // Truncate the buffer based on the actual number of bytes read
                buffer.truncate(read_amount);

                // Further optimization: Create the response with the read bytes and the read
                // amount We will no longer send entire buffer filled with
                // zeroes
                let response = ReadFileResponse {
                    bytes: buffer.into(),
                    read_amount: read_amount as u64,
                };

                Ok(response)
            })
    }

    /// Handles our `readlink_detour`.
    #[tracing::instrument(level = Level::TRACE, skip(self))]
    pub(crate) fn read_link(&mut self, path: PathBuf) -> RemoteResult<ReadLinkFileResponse> {
        let link = self.root.open(
            None,
            &path,
            OFlag::O_PATH | OFlag::O_NOFOLLOW,
            Mode::empty(),
        )?;

        // `readlinkat(2)` fails with `ENOENT` on a descriptor that isn't a symlink, where
        // `readlink(2)` on its path fails with `EINVAL`, which callers such as `realpath` take to
        // mean "not a symlink".
        if fstat(&link).map_err(io::Error::from)?.st_mode & libc::S_IFMT != libc::S_IFLNK {
            return Err(io::Error::from_raw_os_error(libc::EINVAL).into());
        }

        let path = fcntl::readlinkat(&link, "").map_err(io::Error::from)?;

        Ok(ReadLinkFileResponse { path: path.into() })
    }

    #[tracing::instrument(level = "trace", skip(self))]
    pub(crate) fn write_limited(
        &mut self,
        fd: u64,
        start_from: u64,
        buffer: Bytes,
    ) -> RemoteResult<WriteFileResponse> {
        self.open_files
            .get_mut(&fd)
            .ok_or(ResponseError::NotFound(fd))
            .and_then(|remote_file| {
                if let RemoteFile::File(file) = remote_file {
                    let written_amount =
                        file.write_at(&buffer, start_from).map(|written_amount| {
                            WriteFileResponse {
                                written_amount: written_amount as u64,
                            }
                        })?;

                    Ok(written_amount)
                } else {
                    Err(ResponseError::NotFile(fd))
                }
            })
    }

    /// Handles both `mkdir` and `mkdirat`, which has a `dirfd`.
    #[tracing::instrument(level = Level::TRACE, skip(self))]
    pub(crate) fn mkdir(&mut self, dirfd: Option<u64>, path: &Path, mode: u32) -> RemoteResult<()> {
        let base = Self::base(&self.open_files, dirfd, path)?;
        let (parent, name) = self.root.open_parent(base, path)?;

        nix::sys::stat::mkdirat(parent, name, Mode::from_bits_truncate(mode))
            .map_err(io::Error::from)?;

        Ok(())
    }

    /// Handles `unlink`, `rmdir`, and `unlinkat`, which has a `dirfd` and `flags`.
    #[tracing::instrument(level = Level::TRACE, skip(self))]
    pub(crate) fn unlink(
        &mut self,
        dirfd: Option<u64>,
        path: &Path,
        flags: u32,
    ) -> RemoteResult<()> {
        let base = Self::base(&self.open_files, dirfd, path)?;

        let flags = match flags as i32 {
            0 => UnlinkatFlags::NoRemoveDir,
            libc::AT_REMOVEDIR => UnlinkatFlags::RemoveDir,
            _ => {
                return Err(ResponseError::from(std::io::Error::from_raw_os_error(
                    libc::EINVAL,
                )));
            }
        };

        // `rmdir("/")` fails with `EBUSY`. `open_parent` turns `/` into `.`, which
        // `unlinkat(2)` refuses with `EINVAL` instead.
        let bytes = path.as_os_str().as_bytes();
        if matches!(flags, UnlinkatFlags::RemoveDir)
            && bytes.is_empty().not()
            && bytes.iter().all(|byte| *byte == b'/')
        {
            return Err(io::Error::from_raw_os_error(libc::EBUSY).into());
        }

        let (parent, name) = self.root.open_parent(base, path)?;
        nix::unistd::unlinkat(parent, name, flags).map_err(io::Error::from)?;

        Ok(())
    }

    pub(crate) fn ftruncate(&mut self, fd: u64, length: i64) -> RemoteResult<()> {
        let file = self
            .open_files
            .get(&fd)
            .ok_or(ResponseError::NotFound(fd))?;

        match file {
            RemoteFile::File(file) => {
                let result = unsafe { libc::ftruncate(file.as_raw_fd(), length) };
                match result {
                    -1 => Err(ResponseError::from(io::Error::last_os_error())),
                    _ => Ok(()),
                }
            }
            _ => Err(ResponseError::NotFile(fd)),
        }
    }

    pub(crate) fn futimens(&mut self, fd: u64, times: Option<[Timespec; 2]>) -> RemoteResult<()> {
        let file = self
            .open_files
            .get(&fd)
            .ok_or(ResponseError::NotFound(fd))?;

        match file {
            RemoteFile::File(file) => {
                let times = times.map(|times| {
                    [
                        libc::timespec {
                            tv_sec: times[0].tv_sec,
                            tv_nsec: times[0].tv_nsec,
                        },
                        libc::timespec {
                            tv_sec: times[1].tv_sec,
                            tv_nsec: times[1].tv_nsec,
                        },
                    ]
                });
                let result = unsafe {
                    libc::futimens(
                        file.as_raw_fd(),
                        times.map(|times| times.as_ptr()).unwrap_or(ptr::null()),
                    )
                };
                match result {
                    -1 => Err(ResponseError::from(io::Error::last_os_error())),
                    _ => Ok(()),
                }
            }
            _ => Err(ResponseError::NotFile(fd)),
        }
    }

    pub(crate) fn fchown(&mut self, fd: u64, owner: u32, group: u32) -> RemoteResult<()> {
        let file = self
            .open_files
            .get(&fd)
            .ok_or(ResponseError::NotFound(fd))?;

        match file {
            RemoteFile::File(file) => {
                let result = unsafe { libc::fchown(file.as_raw_fd(), owner, group) };
                match result {
                    -1 => Err(ResponseError::from(io::Error::last_os_error())),
                    _ => Ok(()),
                }
            }
            _ => Err(ResponseError::NotFile(fd)),
        }
    }

    pub(crate) fn fchmod(&mut self, fd: u64, mode: u32) -> RemoteResult<()> {
        let file = self
            .open_files
            .get(&fd)
            .ok_or(ResponseError::NotFound(fd))?;

        match file {
            RemoteFile::File(file) => {
                let result = unsafe { libc::fchmod(file.as_raw_fd(), mode) };
                match result {
                    -1 => Err(ResponseError::from(io::Error::last_os_error())),
                    _ => Ok(()),
                }
            }
            _ => Err(ResponseError::NotFile(fd)),
        }
    }

    pub(crate) fn seek(&mut self, fd: u64, seek_from: SeekFrom) -> RemoteResult<SeekFileResponse> {
        trace!(
            "FileManager::seek -> fd {:#?} | seek_from {:#?}",
            fd, seek_from
        );

        self.open_files
            .get_mut(&fd)
            .ok_or(ResponseError::NotFound(fd))
            .and_then(|remote_file| {
                if let RemoteFile::File(file) = remote_file {
                    let seek_result = file
                        .seek(seek_from)
                        .map(|result_offset| SeekFileResponse { result_offset })?;

                    Ok(seek_result)
                } else {
                    Err(ResponseError::NotFile(fd))
                }
            })
    }

    pub(crate) fn write(&mut self, fd: u64, write_bytes: Bytes) -> RemoteResult<WriteFileResponse> {
        trace!(
            "FileManager::write -> fd {:#?} | write_bytes (length) {:#?}",
            fd,
            write_bytes.len()
        );

        self.open_files
            .get_mut(&fd)
            .ok_or(ResponseError::NotFound(fd))
            .and_then(|remote_file| {
                if let RemoteFile::File(file) = remote_file {
                    let write_result =
                        file.write(&write_bytes)
                            .map(|write_amount| WriteFileResponse {
                                written_amount: write_amount as u64,
                            })?;

                    Ok(write_result)
                } else {
                    Err(ResponseError::NotFile(fd))
                }
            })
    }

    /// Always returns `None`, since we don't return any [`FileResponse`] back to mirrord
    /// on `close` of an fd.
    #[tracing::instrument(level = Level::TRACE, skip(self))]
    pub(crate) fn close(&mut self, fd: u64) -> Option<FileResponse> {
        if self.open_files.remove(&fd).is_none() {
            error!(fd, "fd not found!");
        } else {
            OPEN_FD_COUNT.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        }

        None
    }

    /// Always returns `None`, since we don't return any [`FileResponse`] back to mirrord
    /// on `close_dir` of an fd.
    #[tracing::instrument(level = Level::TRACE, skip(self))]
    pub(crate) fn close_dir(&mut self, fd: u64) -> Option<FileResponse> {
        let closed_dir_stream = self.dir_streams.remove(&fd);
        let closed_getdents_stream = self.getdents_streams.remove(&fd);

        if closed_dir_stream.is_some() && closed_getdents_stream.is_some() {
            // Closed `dirstream` and `dentsstream`
            OPEN_FD_COUNT.fetch_sub(2, std::sync::atomic::Ordering::Relaxed);
        } else if closed_dir_stream.is_some() || closed_getdents_stream.is_some() {
            OPEN_FD_COUNT.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        } else {
            error!("FileManager::close_dir -> fd {:#?} not found", fd);
        }

        None
    }

    pub(crate) fn access(
        &mut self,
        pathname: PathBuf,
        mode: u8,
    ) -> RemoteResult<AccessFileResponse> {
        trace!(
            "FileManager::access -> pathname {:#?} | mode {:#?}",
            pathname, mode,
        );
        let file = self
            .root
            .open(None, &pathname, OFlag::O_PATH, Mode::empty())?;

        // Mirror bit representation of flags to support how the flags are represented in the
        // faccess library
        let mode =
            AccessMode::from_bits((mode << 4).reverse_bits() | 1).unwrap_or(AccessMode::EXISTS);

        // `faccessat(2)` can't take an `O_PATH` descriptor before Linux 5.8.
        proc_fd_path(&file)
            .access(mode)
            .map(|_| AccessFileResponse)
            .map_err(ResponseError::from)
    }

    /// Handles `fstat`, which has only an `fd`, and `stat`, `lstat` and `fstatat`, which have a
    /// `path` and possibly a `dirfd`.
    #[tracing::instrument(level = "trace", skip(self))]
    pub(crate) fn xstat(
        &mut self,
        path: Option<PathBuf>,
        fd: Option<u64>,
        follow_symlink: bool,
    ) -> RemoteResult<XstatResponse> {
        // `fstatat(fd, "", AT_EMPTY_PATH)` comes with an empty path, and stats `fd` itself.
        let path = path.filter(|path| fd.is_none() || path.as_os_str().is_empty().not());

        let metadata = match (path, fd) {
            (Some(path), dirfd) => {
                let base = Self::base(&self.open_files, dirfd, &path)?;
                let flags = if follow_symlink {
                    OFlag::O_PATH
                } else {
                    OFlag::O_PATH | OFlag::O_NOFOLLOW
                };

                File::from(self.root.open(base, &path, flags, Mode::empty())?).metadata()?
            }
            (None, Some(fd)) => match self
                .open_files
                .get(&fd)
                .ok_or(ResponseError::NotFound(fd))?
            {
                RemoteFile::File(file) => file.metadata()?,
                RemoteFile::Directory(directory) => directory.file.metadata()?,
            },
            (None, None) => {
                return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput).into());
            }
        };

        Ok(XstatResponse {
            metadata: metadata.into(),
        })
    }

    #[tracing::instrument(level = "trace", skip(self))]
    pub(crate) fn fstatfs(&mut self, fd: u64) -> RemoteResult<XstatFsResponseV2> {
        let file = match self
            .open_files
            .get(&fd)
            .ok_or(ResponseError::NotFound(fd))?
        {
            RemoteFile::File(file) => file,
            RemoteFile::Directory(directory) => &directory.file,
        };

        let statfs = nix::sys::statfs::fstatfs(file).map_err(io::Error::from)?;

        Ok(XstatFsResponseV2 {
            metadata: statfs.into(),
        })
    }

    #[tracing::instrument(level = "trace", skip(self))]
    pub(crate) fn statfs(&mut self, path: PathBuf) -> RemoteResult<XstatFsResponseV2> {
        let file = self.root.open(None, &path, OFlag::O_PATH, Mode::empty())?;
        let statfs = nix::sys::statfs::fstatfs(file).map_err(io::Error::from)?;

        Ok(XstatFsResponseV2 {
            metadata: statfs.into(),
        })
    }

    #[tracing::instrument(level = Level::TRACE, skip(self), ret, err(level = Level::DEBUG))]
    pub(super) fn rename(&mut self, old_path: PathBuf, new_path: PathBuf) -> RemoteResult<()> {
        let (old_parent, old_name) = self.root.open_parent(None, &old_path)?;
        let (new_parent, new_name) = self.root.open_parent(None, &new_path)?;

        fcntl::renameat(old_parent, old_name, new_parent, new_name).map_err(io::Error::from)?;

        Ok(())
    }

    #[tracing::instrument(level = Level::TRACE, skip(self), err(level = Level::DEBUG))]
    pub(crate) fn fdopen_dir(&mut self, fd: u64) -> RemoteResult<OpenDirResponse> {
        let directory = match self
            .open_files
            .get(&fd)
            .ok_or(ResponseError::NotFound(fd))?
        {
            RemoteFile::Directory(directory) => Ok(directory),
            _ => Err(ResponseError::NotDirectory(fd)),
        }?;

        let fd = self
            .fds_iter
            .next()
            .ok_or_else(|| ResponseError::IdsExhausted("fdopen_dir".to_owned()))?;

        let dir_stream = directory.read_dir()?.enumerate();

        if self.dir_streams.insert(fd, dir_stream).is_none() {
            OPEN_FD_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }

        Ok(OpenDirResponse { fd })
    }

    #[tracing::instrument(level = "trace", skip(self))]
    pub(crate) fn get_dir_stream(&mut self, fd: u64) -> RemoteResult<&mut Enumerate<ReadDir>> {
        self.dir_streams
            .get_mut(&fd)
            .ok_or(ResponseError::NotFound(fd))
    }

    fn dot_entry(
        metadata: io::Result<Metadata>,
        position: u64,
        name: &str,
    ) -> io::Result<DirEntryInternal> {
        Ok(DirEntryInternal {
            inode: metadata?.ino(),
            position,
            name: name.to_owned(),
            file_type: DT_DIR,
        })
    }

    /// Get the entries of the current and parent directories, to chain with the iterator
    /// returned by [`Directory::read_dir`].
    fn get_current_and_parent_entries(
        root: &TargetRoot,
        directory: &Directory,
    ) -> VecDeque<io::Result<DirEntryInternal>> {
        let parent = root
            .open_parent_dir(directory)
            .and_then(|parent| File::from(parent).metadata());

        VecDeque::from([
            Self::dot_entry(directory.file.metadata(), 0, "."),
            Self::dot_entry(parent, 1, ".."),
        ])
    }

    /// If a stream does not yet exist for this fd, we create and return it.
    /// The possible remote errors are:
    /// [`ResponseError::NotFound`] if there is not such fd here.
    /// [`ResponseError::NotDirectory`] if the fd points to a file with a non-directory file type.
    #[tracing::instrument(level = Level::TRACE, skip(self))]
    pub(crate) fn get_or_create_getdents64_stream(
        &mut self,
        fd: u64,
    ) -> RemoteResult<&mut Peekable<GetDEnts64Stream>> {
        match self.getdents_streams.entry(fd) {
            Entry::Vacant(e) => match self.open_files.get(&fd) {
                None => Err(ResponseError::NotFound(fd)),
                Some(RemoteFile::File(_file)) => Err(ResponseError::NotDirectory(fd)),
                Some(RemoteFile::Directory(directory)) => {
                    let current_and_parent =
                        Self::get_current_and_parent_entries(&self.root, directory);
                    let stream =
                        GetDEnts64Stream::new(directory.read_dir()?, current_and_parent).peekable();
                    OPEN_FD_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    Ok(e.insert(stream))
                }
            },
            Entry::Occupied(existing) => Ok(existing.into_mut()),
        }
    }

    #[tracing::instrument(level = Level::TRACE, skip(self), ret)]
    pub(crate) fn read_dir(&mut self, fd: u64) -> RemoteResult<ReadDirResponse> {
        let dir_stream = self.get_dir_stream(fd)?;
        let result = match dir_stream.next() {
            Some(offset_entry_pair) => ReadDirResponse {
                direntry: Some(offset_entry_pair.try_into()?),
            },
            _ => ReadDirResponse { direntry: None },
        };

        Ok(result)
    }

    /// Instead of returning just 1 [`DirEntryInternal`] from a `readdir` call (which in
    /// Rust means advancing the [`read_dir`](std::fs::read_dir) iterator), we return
    /// an iterator with (at most) `amount` items.
    #[tracing::instrument(level = Level::TRACE, skip(self), ret)]
    pub(crate) fn read_dir_batch(
        &mut self,
        fd: u64,
        amount: usize,
    ) -> RemoteResult<ReadDirBatchResponse> {
        let result = self
            .get_dir_stream(fd)?
            .take(amount)
            .map(DirEntryInternal::try_from)
            .collect::<Result<Vec<_>, _>>()
            .map(|dir_entries| ReadDirBatchResponse { fd, dir_entries })?;

        Ok(result)
    }

    /// The getdents64 syscall writes dir entries to a buffer, as long as they fit.
    /// If a call did not process all the entries in a dir, the result of the next call continues
    /// where the last one stopped.
    /// After writing all entries, all future calls return 0 entries.
    /// The caller keeps calling until getting 0.
    #[tracing::instrument(level = "trace", skip(self))]
    pub(crate) fn getdents64(
        &mut self,
        fd: u64,
        buffer_size: u64,
    ) -> RemoteResult<GetDEnts64Response> {
        let mut result_size = 0u64;

        // If this is the first call with this fd, the stream will be created, otherwise the
        // existing one is retrieved and we continue from where we stopped on the last call.
        let entry_results = self.get_or_create_getdents64_stream(fd)?;

        // If the stream is empty, it means we've already reached the end in a previous call, so we
        // just return 0 and don't write any entries.
        if entry_results.peek().is_none() {
            // Reached end.
            Ok(GetDEnts64Response {
                fd,
                entries: vec![],
                result_size: 0,
            })
        } else {
            // Trying to allocate according to what the syscall caller allocated.
            // The caller of the syscall allocated buffer_size bytes, so if the average
            // linux_dirent64 in this dir is not bigger than 32 this should be
            // enough. But don't preallocate more than 256 places.
            let initial_vector_capacity = 256.min((buffer_size / 32) as usize);
            let mut entries = Vec::with_capacity(initial_vector_capacity);

            // Peek into the next result, and only consume it if there is room for it in the
            // buffer (and there was no error converting to a
            // `DirEntryInternal`.
            while let Some(entry) = entry_results
                .next_if(|entry_res: &Result<DirEntryInternal, io::Error>| {
                    entry_res.as_ref().is_ok_and(|entry| {
                        entry.get_d_reclen64() as u64 + result_size <= buffer_size
                    })
                })
                .transpose()?
            {
                result_size += entry.get_d_reclen64() as u64;
                entries.push(entry);
            }

            Ok(GetDEnts64Response {
                fd,
                entries,
                result_size,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::symlink};

    use libc::{S_IFLNK, S_IFMT, S_IFREG};
    use rstest::rstest;

    use super::*;
    use crate::util::target_root::Resolver;

    /// Whether the target root is a container's or the agent's own `/`, as for targetless and
    /// workload companion agents. Absolute paths and symlinks resolve against it, so requests
    /// are exercised with both.
    #[derive(Debug, Clone, Copy)]
    enum Mode {
        Targeted,
        Targetless,
    }

    /// A temporary directory holding `a/b/c/` with:
    ///
    /// ```text
    /// file                    regular file
    /// link                 -> file
    /// abs_link             -> <dir>/file
    /// dangling             -> /nonexistent
    /// ```
    ///
    /// For [`Mode::Targeted`] the temporary directory is the target root, so `<dir>` is
    /// `/a/b/c` and `abs_link` only resolves correctly against the target root. For
    /// [`Mode::Targetless`] `<dir>` is the real path on the host.
    ///
    /// Returns the [`FileManager`], resolving paths with `resolver`, and the path to `a/b/c` as
    /// sent by the layer.
    fn setup(mode: Mode, resolver: Resolver) -> (tempfile::TempDir, FileManager, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let host_dir = root.path().join("a/b/c");
        fs::create_dir_all(&host_dir).unwrap();

        let (root_path, dir) = match mode {
            Mode::Targeted => (root.path().to_path_buf(), PathBuf::from("/a/b/c")),
            Mode::Targetless => (PathBuf::from("/"), host_dir.clone()),
        };
        let manager = FileManager::new(TargetRoot::with_resolver(root_path, resolver));

        fs::write(host_dir.join("file"), "hello").unwrap();
        symlink("file", host_dir.join("link")).unwrap();
        symlink(dir.join("file"), host_dir.join("abs_link")).unwrap();
        symlink("/nonexistent", host_dir.join("dangling")).unwrap();

        (root, manager, dir)
    }

    fn file_type(
        manager: &mut FileManager,
        path: impl Into<PathBuf>,
        fd: Option<u64>,
        follow_symlink: bool,
    ) -> RemoteResult<u32> {
        manager
            .xstat(Some(path.into()), fd, follow_symlink)
            .map(|response| response.metadata.mode & S_IFMT)
    }

    const READ: OpenOptionsInternal = OpenOptionsInternal {
        read: true,
        write: false,
        append: false,
        truncate: false,
        create: false,
        create_new: false,
    };

    /// Sends [`FileRequest::OpenRelative`] with a `relative_fd`, [`FileRequest::Open`] without.
    fn open_with(
        manager: &mut FileManager,
        path: impl Into<PathBuf>,
        relative_fd: Option<u64>,
        open_options: OpenOptionsInternal,
    ) -> RemoteResult<u64> {
        let path = path.into();
        let request = match relative_fd {
            Some(relative_fd) => FileRequest::OpenRelative(OpenRelativeFileRequest {
                relative_fd,
                path,
                open_options,
            }),
            None => FileRequest::Open(OpenFileRequest { path, open_options }),
        };

        match manager.handle_message(request).unwrap() {
            Some(FileResponse::Open(result)) => result.map(|response| response.fd),
            response => panic!("unexpected open response: {response:?}"),
        }
    }

    fn open_readonly(manager: &mut FileManager, path: PathBuf, relative_fd: Option<u64>) -> u64 {
        open_with(manager, path, relative_fd, READ).unwrap()
    }

    fn read_all(manager: &mut FileManager, fd: u64) -> Vec<u8> {
        manager.read(fd, 1024).unwrap().bytes.to_vec()
    }

    /// Sends [`FileRequest::MakeDirAt`] with a `dirfd`, [`FileRequest::MakeDir`] without.
    fn make_dir(
        manager: &mut FileManager,
        dirfd: Option<u64>,
        pathname: impl Into<PathBuf>,
    ) -> RemoteResult<()> {
        let pathname = pathname.into();
        let mode = 0o755;
        let request = match dirfd {
            Some(dirfd) => FileRequest::MakeDirAt(MakeDirAtRequest {
                dirfd,
                pathname,
                mode,
            }),
            None => FileRequest::MakeDir(MakeDirRequest { pathname, mode }),
        };

        match manager.handle_message(request).unwrap() {
            Some(FileResponse::MakeDir(result)) => result,
            response => panic!("unexpected mkdir response: {response:?}"),
        }
    }

    fn dir_entry_names(manager: &mut FileManager, fd: u64) -> Vec<String> {
        let mut names = manager
            .getdents64(fd, 64 * 1024)
            .unwrap()
            .entries
            .into_iter()
            .map(|entry| entry.name)
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    fn unlink_at(
        manager: &mut FileManager,
        dirfd: Option<u64>,
        pathname: impl Into<PathBuf>,
        flags: u32,
    ) -> RemoteResult<()> {
        match manager
            .handle_message(FileRequest::UnlinkAt(UnlinkAtRequest {
                dirfd,
                pathname: pathname.into(),
                flags,
            }))
            .unwrap()
        {
            Some(FileResponse::Unlink(result)) => result,
            response => panic!("unexpected unlink response: {response:?}"),
        }
    }

    #[rstest]
    fn unlinkat_preserves_opened_directory(
        #[values(Resolver::detect(), Resolver::Userspace)] resolver: Resolver,
        #[values(Mode::Targeted, Mode::Targetless)] mode: Mode,
        #[values(false, true)] open_relative: bool,
        #[values(0, libc::AT_REMOVEDIR as u32)] flags: u32,
    ) {
        let (root, mut manager, dir) = setup(mode, resolver);
        let host_dir = root.path().join("a/b/c");
        let victim = if flags == 0 { "file" } else { "empty" };
        if flags != 0 {
            fs::create_dir(host_dir.join(victim)).unwrap();
        }
        let fd = if open_relative {
            let parent_fd = open_readonly(&mut manager, dir.parent().unwrap().to_owned(), None);
            open_readonly(&mut manager, PathBuf::from("c"), Some(parent_fd))
        } else {
            open_readonly(&mut manager, dir, None)
        };

        let moved = root.path().join("a/b/moved");
        fs::rename(&host_dir, &moved).unwrap();
        fs::create_dir(&host_dir).unwrap();
        if flags == 0 {
            fs::write(host_dir.join(victim), "replacement").unwrap();
        } else {
            fs::create_dir(host_dir.join(victim)).unwrap();
        }

        unlink_at(&mut manager, Some(fd), victim, flags).unwrap();

        assert_eq!(
            fs::symlink_metadata(moved.join(victim))
                .unwrap_err()
                .raw_os_error(),
            Some(libc::ENOENT)
        );
        if flags == 0 {
            assert_eq!(
                fs::read_to_string(host_dir.join(victim)).unwrap(),
                "replacement"
            );
        } else {
            assert!(host_dir.join(victim).is_dir());
        }
    }

    #[rstest]
    fn unlinkat_does_not_revisit_replaced_ancestor(
        #[values(Resolver::detect(), Resolver::Userspace)] resolver: Resolver,
        #[values(Mode::Targeted, Mode::Targetless)] mode: Mode,
    ) {
        let (root, mut manager, dir) = setup(mode, resolver);
        let fd = open_readonly(&mut manager, dir, None);
        let moved = root.path().join("a/original_b");
        fs::rename(root.path().join("a/b"), &moved).unwrap();
        let decoy = root.path().join("decoy");
        fs::create_dir_all(decoy.join("c")).unwrap();
        fs::write(decoy.join("c/file"), "decoy").unwrap();
        symlink(&decoy, root.path().join("a/b")).unwrap();

        unlink_at(&mut manager, Some(fd), "file", 0).unwrap();

        assert_eq!(
            fs::symlink_metadata(moved.join("c/file"))
                .unwrap_err()
                .raw_os_error(),
            Some(libc::ENOENT)
        );
        assert_eq!(fs::read_to_string(decoy.join("c/file")).unwrap(), "decoy");
    }

    #[rstest]
    fn unlinkat_removes_relative_links_without_following(
        #[values(Resolver::detect(), Resolver::Userspace)] resolver: Resolver,
        #[values(Mode::Targeted, Mode::Targetless)] mode: Mode,
    ) {
        let (root, mut manager, dir) = setup(mode, resolver);
        let fd = open_readonly(&mut manager, dir, None);
        let host_dir = root.path().join("a/b/c");

        for link in ["link", "dangling"] {
            unlink_at(&mut manager, Some(fd), link, 0).unwrap();
            assert_eq!(
                fs::symlink_metadata(host_dir.join(link))
                    .unwrap_err()
                    .raw_os_error(),
                Some(libc::ENOENT)
            );
        }
        assert_eq!(fs::read_to_string(host_dir.join("file")).unwrap(), "hello");
    }

    #[rstest]
    fn unlinkat_absolute_paths_ignore_dirfd_and_use_target_root(
        #[values(Resolver::detect(), Resolver::Userspace)] resolver: Resolver,
    ) {
        let (root, mut manager, dir) = setup(Mode::Targeted, resolver);
        let fd = open_readonly(&mut manager, dir, None);
        let sentinel = tempfile::NamedTempFile::new().unwrap();
        fs::write(sentinel.path(), "host sentinel").unwrap();
        let rooted_sentinel = root.path().join(sentinel.path().strip_prefix("/").unwrap());
        assert!(rooted_sentinel.exists().not());
        fs::create_dir_all(rooted_sentinel.parent().unwrap()).unwrap();

        for dirfd in [None, Some(fd), Some(u64::MAX)] {
            assert_eq!(
                unlink_at(&mut manager, dirfd, sentinel.path(), 0),
                Err(ResponseError::from(io::Error::from_raw_os_error(
                    libc::ENOENT
                )))
            );
            assert_eq!(
                fs::read_to_string(sentinel.path()).unwrap(),
                "host sentinel"
            );

            fs::write(&rooted_sentinel, "target sentinel").unwrap();
            unlink_at(&mut manager, dirfd, sentinel.path(), 0).unwrap();
            assert!(rooted_sentinel.exists().not());
            assert_eq!(
                fs::read_to_string(sentinel.path()).unwrap(),
                "host sentinel"
            );
        }
    }

    #[rstest]
    fn unlinkat_relative_errors_preserve_files(
        #[values(Resolver::detect(), Resolver::Userspace)] resolver: Resolver,
        #[values(Mode::Targeted, Mode::Targetless)] mode: Mode,
    ) {
        let (root, mut manager, dir) = setup(mode, resolver);
        let fd = open_readonly(&mut manager, dir.clone(), None);
        let file_fd = open_readonly(&mut manager, dir.join("file"), None);
        let closed_fd = open_readonly(&mut manager, dir, None);
        assert!(
            manager
                .handle_message(FileRequest::Close(CloseFileRequest { fd: closed_fd }))
                .unwrap()
                .is_none()
        );

        for (dirfd, path, flags, expected) in [
            (u64::MAX, "file", 0, ResponseError::NotFound(u64::MAX)),
            (closed_fd, "file", 0, ResponseError::NotFound(closed_fd)),
            (file_fd, "file", 0, ResponseError::NotDirectory(file_fd)),
            (
                fd,
                "file",
                libc::AT_REMOVEDIR as u32 | 1,
                ResponseError::from(io::Error::from_raw_os_error(libc::EINVAL)),
            ),
            (
                fd,
                "missing",
                0,
                ResponseError::from(io::Error::from_raw_os_error(libc::ENOENT)),
            ),
        ] {
            assert_eq!(
                unlink_at(&mut manager, Some(dirfd), path, flags),
                Err(expected)
            );
            assert_eq!(
                fs::read_to_string(root.path().join("a/b/c/file")).unwrap(),
                "hello"
            );
        }
    }

    #[rstest]
    fn fstatat_relative_to_dir_fd(
        #[values(Resolver::detect(), Resolver::Userspace)] resolver: Resolver,
        #[values(Mode::Targeted, Mode::Targetless)] mode: Mode,
    ) {
        let (_root, mut manager, dir) = setup(mode, resolver);
        let fd = Some(open_readonly(&mut manager, dir, None));

        assert_eq!(file_type(&mut manager, "file", fd, true).unwrap(), S_IFREG);
        assert_eq!(file_type(&mut manager, "link", fd, true).unwrap(), S_IFREG);
        assert_eq!(file_type(&mut manager, "link", fd, false).unwrap(), S_IFLNK);
        assert_eq!(
            file_type(&mut manager, "abs_link", fd, true).unwrap(),
            S_IFREG
        );
        assert_eq!(
            file_type(&mut manager, "dangling", fd, false).unwrap(),
            S_IFLNK
        );
        assert!(file_type(&mut manager, "dangling", fd, true).is_err());
    }

    #[rstest]
    fn stat_absolute_path(
        #[values(Resolver::detect(), Resolver::Userspace)] resolver: Resolver,
        #[values(Mode::Targeted, Mode::Targetless)] mode: Mode,
    ) {
        let (_root, mut manager, dir) = setup(mode, resolver);

        assert_eq!(
            file_type(&mut manager, dir.join("file"), None, true).unwrap(),
            S_IFREG
        );
        assert_eq!(
            file_type(&mut manager, dir.join("link"), None, true).unwrap(),
            S_IFREG
        );
        assert_eq!(
            file_type(&mut manager, dir.join("link"), None, false).unwrap(),
            S_IFLNK
        );
        assert_eq!(
            file_type(&mut manager, dir.join("abs_link"), None, true).unwrap(),
            S_IFREG
        );
        assert_eq!(
            file_type(&mut manager, dir.join("dangling"), None, false).unwrap(),
            S_IFLNK
        );
    }

    /// A target root next to an `outside` directory, which stands for the rest of the agent's
    /// filesystem (the node's, for a targeted agent), and links that lead out of the target's
    /// root when the agent's own root resolves them:
    ///
    /// ```text
    /// outside/secret              regular file
    /// outside/link             -> secret
    /// outside/empty/              empty directory
    /// root/a/b/c/file             regular file
    /// root/a/b/c/abs_secret    -> <outside>/secret
    /// root/a/b/c/abs_escape    -> <outside>
    /// root/a/b/c/rel_escape    -> ../../../../outside
    /// ```
    struct Escape {
        _parent: tempfile::TempDir,
        root: PathBuf,
        outside: PathBuf,
        manager: FileManager,
        /// `/a/b/c`, opened by the client.
        dir_fd: u64,
    }

    impl Escape {
        fn new(resolver: Resolver) -> Self {
            let parent = tempfile::tempdir().unwrap();
            let root = parent.path().join("root");
            let outside = parent.path().join("outside");
            let dir = root.join("a/b/c");
            fs::create_dir_all(&dir).unwrap();
            fs::create_dir_all(outside.join("empty")).unwrap();
            fs::write(outside.join("secret"), "secret").unwrap();
            symlink("secret", outside.join("link")).unwrap();
            fs::write(dir.join("file"), "hello").unwrap();
            symlink(outside.join("secret"), dir.join("abs_secret")).unwrap();
            symlink(&outside, dir.join("abs_escape")).unwrap();
            symlink("../../../../outside", dir.join("rel_escape")).unwrap();

            let mut manager = FileManager::new(TargetRoot::with_resolver(root.clone(), resolver));
            let dir_fd = open_readonly(&mut manager, PathBuf::from("/a/b/c"), None);

            Self {
                _parent: parent,
                root,
                outside,
                manager,
                dir_fd,
            }
        }

        /// The ways a client can name `outside`, each with the `dirfd` to send it with. An
        /// absolute path goes both with and without a `dirfd`, which it has to ignore.
        fn outside_dirs(&self) -> Vec<(Option<u64>, PathBuf)> {
            let absolute = [
                self.outside.clone(),
                PathBuf::from("/a/b/c/abs_escape"),
                PathBuf::from("/a/b/c/rel_escape"),
                PathBuf::from("/a/b/c/../../../../outside"),
            ];
            let relative = ["abs_escape", "rel_escape", "../../../../outside"];

            absolute
                .into_iter()
                .flat_map(|path| [(None, path.clone()), (Some(self.dir_fd), path)])
                .chain(
                    relative
                        .into_iter()
                        .map(|path| (Some(self.dir_fd), PathBuf::from(path))),
                )
                .collect()
        }

        /// The ways a client can name `outside/secret`, as in [`Self::outside_dirs`].
        fn outside_secrets(&self) -> Vec<(Option<u64>, PathBuf)> {
            self.outside_dirs()
                .into_iter()
                .map(|(dirfd, dir)| (dirfd, dir.join("secret")))
                .chain([
                    (None, PathBuf::from("/a/b/c/abs_secret")),
                    (Some(self.dir_fd), PathBuf::from("/a/b/c/abs_secret")),
                    (Some(self.dir_fd), PathBuf::from("abs_secret")),
                ])
                .collect()
        }

        fn assert_outside_untouched(&self) {
            let mut names = fs::read_dir(&self.outside)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .collect::<Vec<_>>();
            names.sort();
            assert_eq!(names, ["empty", "link", "secret"]);
            assert_eq!(
                fs::read_to_string(self.outside.join("secret")).unwrap(),
                "secret"
            );
            assert_eq!(
                fs::read_link(self.outside.join("link")).unwrap(),
                Path::new("secret")
            );
            assert!(
                fs::read_dir(self.outside.join("empty"))
                    .unwrap()
                    .next()
                    .is_none()
            );
        }
    }

    /// An absolute path sent to `OpenRelative` with a directory's fd resolves from the target
    /// root, the way `openat(2)` ignores its `dirfd` for one, even when the directory is `/`.
    #[rstest]
    fn open_relative_absolute_path_stays_in_target_root(
        #[values(Resolver::detect(), Resolver::Userspace)] resolver: Resolver,
    ) {
        let mut escape = Escape::new(resolver);
        let root_fd = open_readonly(&mut escape.manager, PathBuf::from("/"), None);
        let secret = escape.outside.join("secret");

        assert_eq!(
            open_with(&mut escape.manager, &secret, Some(root_fd), READ),
            Err(ResponseError::from(io::Error::from_raw_os_error(
                libc::ENOENT
            )))
        );

        let in_target = escape.root.join(secret.strip_prefix("/").unwrap());
        fs::create_dir_all(in_target.parent().unwrap()).unwrap();
        fs::write(&in_target, "target secret").unwrap();
        let fd = open_with(&mut escape.manager, &secret, Some(root_fd), READ).unwrap();
        assert_eq!(read_all(&mut escape.manager, fd), b"target secret");
    }

    #[rstest]
    fn escaping_paths_read_nothing_outside_target_root(
        #[values(Resolver::detect(), Resolver::Userspace)] resolver: Resolver,
    ) {
        let mut escape = Escape::new(resolver);
        let secret_inode = fs::metadata(escape.outside.join("secret")).unwrap().ino();
        let outside_inode = fs::metadata(&escape.outside).unwrap().ino();

        for (dirfd, path) in escape.outside_dirs() {
            let opened = open_with(&mut escape.manager, &path, dirfd, READ);
            assert!(opened.is_err(), "{path:?} from {dirfd:?}: {opened:?}");
            let stat = escape.manager.xstat(Some(path.clone()), dirfd, true);
            assert!(stat.is_err(), "{path:?} from {dirfd:?}: {stat:?}");

            if dirfd.is_none() {
                let statfs = escape.manager.statfs(path.clone());
                assert!(statfs.is_err(), "{path:?}: {statfs:?}");
                let link = escape.manager.read_link(path.join("link"));
                assert!(link.is_err(), "{path:?}: {link:?}");
            }
        }

        for (dirfd, path) in escape.outside_secrets() {
            let opened = open_with(&mut escape.manager, &path, dirfd, READ);
            assert!(opened.is_err(), "{path:?} from {dirfd:?}: {opened:?}");
            let stat = escape.manager.xstat(Some(path.clone()), dirfd, true);
            assert!(stat.is_err(), "{path:?} from {dirfd:?}: {stat:?}");

            // `abs_secret` itself is a link inside the target.
            if let Ok(lstat) = escape.manager.xstat(Some(path.clone()), dirfd, false) {
                assert_ne!(
                    lstat.metadata.inode, secret_inode,
                    "{path:?} from {dirfd:?}"
                );
                assert_ne!(
                    lstat.metadata.inode, outside_inode,
                    "{path:?} from {dirfd:?}"
                );
            }

            if dirfd.is_none() {
                let access = escape.manager.access(path.clone(), 0);
                assert!(access.is_err(), "{path:?}: {access:?}");
            }
        }
    }

    #[rstest]
    fn escaping_paths_modify_nothing_outside_target_root(
        #[values(Resolver::detect(), Resolver::Userspace)] resolver: Resolver,
    ) {
        let mut escape = Escape::new(resolver);
        let write = |truncate, create, create_new| OpenOptionsInternal {
            write: true,
            truncate,
            create,
            create_new,
            ..READ
        };

        for (dirfd, path) in escape.outside_secrets() {
            let opened = open_with(&mut escape.manager, &path, dirfd, write(true, false, false));
            assert!(opened.is_err(), "{path:?} from {dirfd:?}: {opened:?}");
        }

        for (dirfd, dir) in escape.outside_dirs() {
            let secret = dir.join("secret");
            let planted = dir.join("planted");

            for options in [
                write(false, true, false),
                write(true, true, false),
                write(false, false, true),
            ] {
                let opened = open_with(&mut escape.manager, &planted, dirfd, options);
                assert!(opened.is_err(), "{planted:?} from {dirfd:?}: {opened:?}");
            }
            let made = make_dir(&mut escape.manager, dirfd, &planted);
            assert!(made.is_err(), "{planted:?} from {dirfd:?}: {made:?}");
            let unlinked = unlink_at(&mut escape.manager, dirfd, &secret, 0);
            assert!(unlinked.is_err(), "{secret:?} from {dirfd:?}: {unlinked:?}");
            let removed = unlink_at(
                &mut escape.manager,
                dirfd,
                dir.join("empty"),
                libc::AT_REMOVEDIR as u32,
            );
            assert!(removed.is_err(), "{dir:?} from {dirfd:?}: {removed:?}");

            if dirfd.is_none() {
                let renamed = escape.manager.rename(secret.clone(), "/stolen".into());
                assert!(renamed.is_err(), "{secret:?}: {renamed:?}");
                let renamed = escape.manager.rename("/a/b/c/file".into(), planted.clone());
                assert!(renamed.is_err(), "{planted:?}: {renamed:?}");
            }

            escape.assert_outside_untouched();
        }

        assert_eq!(
            fs::read_to_string(escape.root.join("a/b/c/file")).unwrap(),
            "hello"
        );
        assert!(escape.root.join("stolen").exists().not());
    }

    /// An opened directory whose path is swapped for a link out of the target root: requests on
    /// its fd act on the directory that was opened, and a path that leaves it resolves from the
    /// target root, where the link leads nowhere.
    #[rstest]
    fn opened_directory_is_not_reopened_by_path(
        #[values(Resolver::detect(), Resolver::Userspace)] resolver: Resolver,
    ) {
        let mut escape = Escape::new(resolver);
        let fd = escape.dir_fd;
        let dir = escape.root.join("a/b/c");
        let inode = fs::metadata(&dir).unwrap().ino();
        fs::rename(&dir, escape.root.join("a/b/moved")).unwrap();
        symlink(&escape.outside, &dir).unwrap();

        let entries = [".", "..", "abs_escape", "abs_secret", "file", "rel_escape"];
        assert_eq!(dir_entry_names(&mut escape.manager, fd), entries);

        let stream = escape.manager.fdopen_dir(fd).unwrap().fd;
        let mut names = escape
            .manager
            .read_dir_batch(stream, 64)
            .unwrap()
            .dir_entries
            .into_iter()
            .map(|entry| entry.name)
            .collect::<Vec<_>>();
        names.sort();
        assert_eq!(names, entries[2..]);

        let stat = escape.manager.xstat(None, Some(fd), true).unwrap();
        assert_eq!(stat.metadata.inode, inode);
        assert!(escape.manager.fstatfs(fd).is_ok());

        let file = open_readonly(&mut escape.manager, PathBuf::from("file"), Some(fd));
        assert_eq!(read_all(&mut escape.manager, file), b"hello");

        let opened = open_with(&mut escape.manager, "../c/secret", Some(fd), READ);
        assert!(opened.is_err(), "{opened:?}");
        let made = make_dir(&mut escape.manager, Some(fd), "../c/planted");
        assert!(made.is_err(), "{made:?}");
        let unlinked = unlink_at(&mut escape.manager, Some(fd), "../c/secret", 0);
        assert!(unlinked.is_err(), "{unlinked:?}");

        escape.assert_outside_untouched();
    }

    /// A relative path that leaves an opened directory resolves from where the directory is now,
    /// not from the path that opened it, as `*at(2)` syscalls do.
    #[rstest]
    fn relative_paths_leave_renamed_directory_from_where_it_is(
        #[values(Resolver::detect(), Resolver::Userspace)] resolver: Resolver,
        #[values(Mode::Targeted, Mode::Targetless)] mode: Mode,
    ) {
        let (root, mut manager, dir) = setup(mode, resolver);
        let fd = open_readonly(&mut manager, dir, None);
        let old_parent = root.path().join("a/b");
        let new_parent = root.path().join("x");
        fs::create_dir(&new_parent).unwrap();
        fs::rename(old_parent.join("c"), new_parent.join("c")).unwrap();
        fs::create_dir(old_parent.join("c")).unwrap();
        fs::write(old_parent.join("sibling"), "old").unwrap();
        fs::write(new_parent.join("sibling"), "new").unwrap();

        let file = open_readonly(&mut manager, PathBuf::from("../sibling"), Some(fd));
        assert_eq!(read_all(&mut manager, file), b"new");
        let file = open_readonly(&mut manager, PathBuf::from("../c/file"), Some(fd));
        assert_eq!(read_all(&mut manager, file), b"hello");

        unlink_at(&mut manager, Some(fd), "../sibling", 0).unwrap();
        assert!(new_parent.join("sibling").exists().not());
        assert_eq!(
            fs::read_to_string(old_parent.join("sibling")).unwrap(),
            "old"
        );

        make_dir(&mut manager, Some(fd), "../made").unwrap();
        assert!(new_parent.join("made").is_dir());
        assert!(old_parent.join("made").exists().not());
    }

    /// An opened directory that something outside the target moved out of the target root,
    /// which happens to a directory in a volume that the target mounts only part of: its `..`
    /// leads out of the target root without meeting it, so a relative path that leaves it
    /// fails.
    #[rstest]
    fn relative_paths_leave_no_directory_moved_out_of_target_root(
        #[values(Resolver::detect(), Resolver::Userspace)] resolver: Resolver,
    ) {
        let mut escape = Escape::new(resolver);
        let fd = escape.dir_fd;
        fs::rename(
            escape.root.join("a/b/c"),
            escape.root.parent().unwrap().join("moved"),
        )
        .unwrap();

        let file = open_readonly(&mut escape.manager, PathBuf::from("file"), Some(fd));
        assert_eq!(read_all(&mut escape.manager, file), b"hello");

        let opened = open_with(&mut escape.manager, "../outside/secret", Some(fd), READ);
        assert!(opened.is_err(), "{opened:?}");
        let stat = escape
            .manager
            .xstat(Some("../outside/secret".into()), Some(fd), true);
        assert!(stat.is_err(), "{stat:?}");
        let made = make_dir(&mut escape.manager, Some(fd), "../outside/planted");
        assert!(made.is_err(), "{made:?}");
        let unlinked = unlink_at(&mut escape.manager, Some(fd), "../outside/secret", 0);
        assert!(unlinked.is_err(), "{unlinked:?}");

        escape.assert_outside_untouched();
    }

    /// Without a target, the agent's own procfs is reachable, and its magic links lead anywhere.
    #[rstest]
    fn magic_links_are_not_followed(
        #[values(Resolver::detect(), Resolver::Userspace)] resolver: Resolver,
    ) {
        let (_root, mut manager, dir) = setup(Mode::Targetless, resolver);
        let file = dir.join("file");
        symlink("/proc/self/root", dir.join("magic")).unwrap();
        let held = File::open(&file).unwrap();

        for path in [
            Path::new("/proc/self/root").join(file.strip_prefix("/").unwrap()),
            dir.join("magic").join(file.strip_prefix("/").unwrap()),
            PathBuf::from(format!("/proc/self/fd/{}", held.as_raw_fd())),
        ] {
            assert_eq!(
                open_with(&mut manager, &path, None, READ),
                Err(ResponseError::from(io::Error::from_raw_os_error(
                    libc::ELOOP
                ))),
                "{path:?}"
            );
        }
    }

    /// The layer resolves relative paths locally, unless it can send them with a remote
    /// directory, so a relative path without one is rejected rather than resolved from the root.
    #[rstest]
    fn relative_paths_without_dirfd_are_rejected(
        #[values(Resolver::detect(), Resolver::Userspace)] resolver: Resolver,
    ) {
        let (root, mut manager, _dir) = setup(Mode::Targeted, resolver);
        let file = PathBuf::from("a/b/c/file");
        let einval = || ResponseError::from(io::Error::from_raw_os_error(libc::EINVAL));

        assert_eq!(open_with(&mut manager, &file, None, READ), Err(einval()));
        assert_eq!(manager.xstat(Some(file.clone()), None, true), Err(einval()));
        assert_eq!(
            manager.xstat(Some(file.clone()), None, false),
            Err(einval())
        );
        assert_eq!(manager.statfs(file.clone()), Err(einval()));
        assert_eq!(manager.access(file.clone(), 0), Err(einval()));
        assert_eq!(
            manager.read_link(PathBuf::from("a/b/c/link")),
            Err(einval())
        );
        assert_eq!(make_dir(&mut manager, None, "a/new"), Err(einval()));
        assert_eq!(unlink_at(&mut manager, None, &file, 0), Err(einval()));
        assert_eq!(
            unlink_at(&mut manager, None, "a/b/c", libc::AT_REMOVEDIR as u32),
            Err(einval())
        );
        assert_eq!(manager.rename(file.clone(), "/moved".into()), Err(einval()));
        assert_eq!(
            manager.rename("/a/b/c/file".into(), "moved".into()),
            Err(einval())
        );

        assert_eq!(
            fs::read_to_string(root.path().join("a/b/c/file")).unwrap(),
            "hello"
        );
        assert!(root.path().join("a/new").exists().not());
    }

    /// `O_CREAT` follows a dangling symlink and creates its target, which for an absolute
    /// symlink is inside the target root.
    #[rstest]
    fn create_through_dangling_symlink_stays_in_target_root(
        #[values(Resolver::detect(), Resolver::Userspace)] resolver: Resolver,
        #[values(false, true)] relative: bool,
    ) {
        let (root, mut manager, dir) = setup(Mode::Targeted, resolver);
        let create = OpenOptionsInternal {
            write: true,
            create: true,
            ..READ
        };

        let fd = if relative {
            let dir_fd = open_readonly(&mut manager, dir, None);
            open_with(&mut manager, "dangling", Some(dir_fd), create)
        } else {
            open_with(&mut manager, dir.join("dangling"), None, create)
        }
        .unwrap();
        manager.write(fd, Bytes::from_static(b"created")).unwrap();

        assert_eq!(
            fs::read_to_string(root.path().join("nonexistent")).unwrap(),
            "created"
        );
        assert!(Path::new("/nonexistent").exists().not());
    }
}
