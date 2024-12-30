use std::{
    self,
    collections::{hash_map::Entry, HashMap, VecDeque},
    fs::{read_link, File, OpenOptions, ReadDir},
    io::{self, prelude::*, BufReader, SeekFrom},
    iter::{Enumerate, Peekable},
    ops::RangeInclusive,
    os::unix::{fs::MetadataExt, prelude::FileExt},
    path::{Path, PathBuf},
};

use faccess::{AccessMode, PathExt};
use libc::DT_DIR;
use mirrord_protocol::{file::*, FileRequest, FileResponse, RemoteResult, ResponseError};
use tracing::{error, trace, Level};

use crate::{error::AgentResult, metrics::OPEN_FD_COUNT};

#[derive(Debug)]
pub enum RemoteFile {
    File(File),
    Directory(PathBuf),
}

fn log_err(entry_res: io::Result<DirEntryInternal>) -> io::Result<DirEntryInternal> {
    entry_res.inspect_err(|err| error!("Converting DirEntry failed with {err:?}"))
}

#[derive(Debug)]
struct GetDEnts64Stream {
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
            .map(log_err);
        self.current_index += 1;
        ret
    }
}

#[derive(Debug)]
pub(crate) struct FileManager {
    root_path: PathBuf,
    open_files: HashMap<u64, RemoteFile>,
    dir_streams: HashMap<u64, Enumerate<ReadDir>>,
    getdents_streams: HashMap<u64, Peekable<GetDEnts64Stream>>,
    fds_iter: RangeInclusive<u64>,
}

pub fn get_root_path_from_optional_pid(pid: Option<u64>) -> PathBuf {
    match pid {
        Some(pid) => PathBuf::from("/proc").join(pid.to_string()).join("root"),
        None => PathBuf::from("/"),
    }
}

/// Resolve a path that might contain symlinks from a specific container to a path accessible from
/// the root host
#[tracing::instrument(level = "trace")]
pub fn resolve_path<P: AsRef<Path> + std::fmt::Debug, R: AsRef<Path> + std::fmt::Debug>(
    path: P,
    root_path: R,
) -> std::io::Result<PathBuf> {
    use std::path::Component::*;

    let mut temp_path = PathBuf::new();
    for component in path.as_ref().components() {
        match component {
            RootDir => {}
            Prefix(prefix) => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("Prefix not supported {prefix:?}"),
            ))?,
            CurDir => {}
            ParentDir => {
                if !temp_path.pop() {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "LFI attempt?",
                    ))?
                }
            }
            Normal(component) => {
                let real_path = root_path.as_ref().join(&temp_path).join(component);
                if real_path.is_symlink() {
                    trace!("{:?} is symlink", real_path);
                    let sym_dest = real_path.read_link()?;
                    temp_path = temp_path.join(sym_dest);
                } else {
                    temp_path = temp_path.join(component);
                }
                if temp_path.has_root() {
                    temp_path = temp_path
                        .strip_prefix("/")
                        .map_err(|_| {
                            std::io::Error::new(
                                std::io::ErrorKind::InvalidInput,
                                "couldn't strip prefix",
                            )
                        })?
                        .into();
                }
            }
        }
    }
    // full path, from host perspective
    let final_path = root_path.as_ref().join(temp_path);
    Ok(final_path)
}

impl FileManager {
    /// Executes the request and returns the response.
    #[tracing::instrument(level = Level::TRACE, skip(self), err)]
    pub(crate) fn handle_message(
        &mut self,
        request: FileRequest,
    ) -> AgentResult<Option<FileResponse>> {
        Ok(match request {
            FileRequest::Open(OpenFileRequest { path, open_options }) => {
                // TODO: maybe not agent error on this?
                let path = path
                    .strip_prefix("/")
                    .inspect_err(|fail| error!("file_worker -> {:#?}", fail))?;

                let open_result = self.open(path.into(), open_options);
                Some(FileResponse::Open(open_result))
            }
            FileRequest::OpenRelative(OpenRelativeFileRequest {
                relative_fd,
                path,
                open_options,
            }) => {
                let open_result = self.open_relative(relative_fd, path, open_options);
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
                let write_result = self.write(fd, write_bytes);
                Some(FileResponse::Write(write_result))
            }
            FileRequest::WriteLimited(WriteLimitedFileRequest {
                remote_fd,
                start_from,
                write_bytes,
            }) => {
                let write_result = self.write_limited(remote_fd, start_from, write_bytes);
                Some(FileResponse::WriteLimited(write_result))
            }
            FileRequest::Close(CloseFileRequest { fd }) => self.close(fd),
            FileRequest::Access(AccessFileRequest { pathname, mode }) => {
                let pathname = pathname
                    .strip_prefix("/")
                    .inspect_err(|fail| error!("file_worker -> {:#?}", fail))?;

                let access_result = self.access(pathname.into(), mode);
                Some(FileResponse::Access(access_result))
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
                let xstat_result = self.xstatfs(fd);
                Some(FileResponse::XstatFs(xstat_result))
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
                Some(FileResponse::MakeDir(self.mkdir(&pathname, mode)))
            }
            FileRequest::MakeDirAt(MakeDirAtRequest {
                dirfd,
                pathname,
                mode,
            }) => Some(FileResponse::MakeDir(self.mkdirat(dirfd, &pathname, mode))),
        })
    }

    #[tracing::instrument(level = Level::TRACE)]
    pub fn new(pid: Option<u64>) -> Self {
        let root_path = get_root_path_from_optional_pid(pid);
        trace!("Agent root path >> {root_path:?}");

        Self {
            root_path,
            open_files: Default::default(),
            dir_streams: Default::default(),
            getdents_streams: Default::default(),
            fds_iter: (0..=u64::MAX),
        }
    }

    #[tracing::instrument(level = Level::TRACE, skip(self), err(level = Level::DEBUG))]
    fn open(
        &mut self,
        path: PathBuf,
        open_options: OpenOptionsInternal,
    ) -> RemoteResult<OpenFileResponse> {
        let path = resolve_path(path, &self.root_path)?;
        let file = OpenOptions::from(open_options).open(&path)?;

        let fd = self
            .fds_iter
            .next()
            .ok_or_else(|| ResponseError::IdsExhausted("open".to_string()))?;

        let metadata = file.metadata()?;

        let remote_file = if metadata.is_dir() {
            RemoteFile::Directory(path)
        } else {
            RemoteFile::File(file)
        };

        if self.open_files.insert(fd, remote_file).is_none() {
            OPEN_FD_COUNT.inc();
        }

        Ok(OpenFileResponse { fd })
    }

    #[tracing::instrument(level = Level::TRACE, skip(self), err(level = Level::DEBUG))]
    fn open_relative(
        &mut self,
        relative_fd: u64,
        path: PathBuf,
        open_options: OpenOptionsInternal,
    ) -> RemoteResult<OpenFileResponse> {
        let relative_dir = self
            .open_files
            .get(&relative_fd)
            .ok_or(ResponseError::NotFound(relative_fd))?;

        if let RemoteFile::Directory(relative_dir) = relative_dir {
            let path = relative_dir.join(&path);

            let file = OpenOptions::from(open_options).open(&path)?;

            let fd = self.fds_iter.next().ok_or_else(|| {
                ResponseError::IdsExhausted("FileManager::open_relative".to_string())
            })?;

            let metadata = file.metadata()?;

            let remote_file = if metadata.is_dir() {
                RemoteFile::Directory(path)
            } else {
                RemoteFile::File(file)
            };

            if self.open_files.insert(fd, remote_file).is_none() {
                OPEN_FD_COUNT.inc();
            }

            Ok(OpenFileResponse { fd })
        } else {
            Err(ResponseError::NotDirectory(relative_fd))
        }
    }

    #[tracing::instrument(level = "trace", skip(self))]
    pub(crate) fn read(&mut self, fd: u64, buffer_size: u64) -> RemoteResult<ReadFileResponse> {
        self.open_files
            .get_mut(&fd)
            .ok_or(ResponseError::NotFound(fd))
            .and_then(|remote_file| {
                if let RemoteFile::File(file) = remote_file {
                    let mut buffer = vec![0; buffer_size as usize];
                    let read_amount = file.read(&mut buffer)?;

                    // Truncate the buffer based on the actual number of bytes read
                    buffer.truncate(read_amount);

                    // Create the response with the read bytes and the read amount
                    let response = ReadFileResponse {
                        bytes: buffer,
                        read_amount: read_amount as u64,
                    };

                    Ok(response)
                } else {
                    Err(ResponseError::NotFile(fd))
                }
            })
    }

    /// Remote implementation of `fgets`.
    ///
    /// Uses `BufReader::read_line` to read a line (including `"\n"`) from a file with `fd`. The
    /// file cursor position has to be moved manually due to this.
    ///
    /// `fgets` is only supposed to read `buffer_size`, so we limit moving the file's position based
    /// on it (even though we return the full `Vec` of bytes).
    #[tracing::instrument(level = "trace", skip(self))]
    pub(crate) fn read_line(
        &mut self,
        fd: u64,
        buffer_size: u64,
    ) -> RemoteResult<ReadFileResponse> {
        self.open_files
            .get_mut(&fd)
            .ok_or(ResponseError::NotFound(fd))
            .and_then(|remote_file| {
                if let RemoteFile::File(file) = remote_file {
                    let original_position = file.stream_position()?;
                    // limit bytes read using take
                    let mut reader = BufReader::new(std::io::Read::by_ref(file)).take(buffer_size);
                    let mut buffer = Vec::<u8>::with_capacity(buffer_size as usize);
                    Ok(reader
                        .read_until(b'\n', &mut buffer)
                        .and_then(|read_amount| {
                            // Revert file to original position + bytes read (in case the
                            // bufreader advanced too much)
                            file.seek(SeekFrom::Start(original_position + read_amount as u64))?;

                            // We handle the extra bytes in the `fgets` hook, so here we can
                            // just return the full buffer.
                            let response = ReadFileResponse {
                                bytes: buffer,
                                read_amount: read_amount as u64,
                            };

                            Ok(response)
                        })?)
                } else {
                    Err(ResponseError::NotFile(fd))
                }
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
            .and_then(|remote_file| {
                if let RemoteFile::File(file) = remote_file {
                    let mut buffer = vec![0; buffer_size as usize];

                    let read_amount = file.read_at(&mut buffer, start_from)?;

                    // Truncate the buffer based on the actual number of bytes read
                    buffer.truncate(read_amount);

                    // Further optimization: Create the response with the read bytes and the read
                    // amount We will no longer send entire buffer filled with
                    // zeroes
                    let response = ReadFileResponse {
                        bytes: buffer,
                        read_amount: read_amount as u64,
                    };

                    Ok(response)
                } else {
                    Err(ResponseError::NotFile(fd))
                }
            })
    }

    /// Handles our `readlink_detour` with [`std::fs::read_link`].
    #[tracing::instrument(level = Level::TRACE, skip_all)]
    pub(crate) fn read_link(&mut self, path: PathBuf) -> RemoteResult<ReadLinkFileResponse> {
        let path = path
            .strip_prefix("/")
            .inspect_err(|fail| error!("file_worker -> {:#?}", fail))?;

        let full_path = self.root_path.join(path);

        read_link(full_path)
            .map(|path| ReadLinkFileResponse { path })
            .map_err(ResponseError::from)
    }

    #[tracing::instrument(level = "trace", skip(self))]
    pub(crate) fn write_limited(
        &mut self,
        fd: u64,
        start_from: u64,
        buffer: Vec<u8>,
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

    pub(crate) fn mkdir(&mut self, path: &Path, mode: u32) -> RemoteResult<()> {
        trace!("FileManager::mkdir -> path {:#?} | mode {:#?}", path, mode);

        let path = resolve_path(path, &self.root_path)?;

        match nix::unistd::mkdir(&path, nix::sys::stat::Mode::from_bits_truncate(mode)) {
            Ok(_) => Ok(()),
            Err(err) => Err(ResponseError::from(std::io::Error::from_raw_os_error(
                err as i32,
            ))),
        }
    }

    pub(crate) fn mkdirat(&mut self, dirfd: u64, path: &Path, mode: u32) -> RemoteResult<()> {
        trace!(
            "FileManager::mkdirat -> dirfd {:#?} | path {:#?} | mode {:#?}",
            dirfd,
            path,
            mode
        );

        let relative_dir = self
            .open_files
            .get(&dirfd)
            .ok_or(ResponseError::NotFound(dirfd))?;

        if let RemoteFile::Directory(relative_dir) = relative_dir {
            let path = relative_dir.join(path);

            match nix::unistd::mkdir(&path, nix::sys::stat::Mode::from_bits_truncate(mode)) {
                Ok(_) => Ok(()),
                Err(err) => Err(ResponseError::from(std::io::Error::from_raw_os_error(
                    err as i32,
                ))),
            }
        } else {
            Err(ResponseError::NotDirectory(dirfd))
        }
    }

    pub(crate) fn seek(&mut self, fd: u64, seek_from: SeekFrom) -> RemoteResult<SeekFileResponse> {
        trace!(
            "FileManager::seek -> fd {:#?} | seek_from {:#?}",
            fd,
            seek_from
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

    pub(crate) fn write(
        &mut self,
        fd: u64,
        write_bytes: Vec<u8>,
    ) -> RemoteResult<WriteFileResponse> {
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
            OPEN_FD_COUNT.dec();
        }

        None
    }

    /// Always returns `None`, since we don't return any [`FileResponse`] back to mirrord
    /// on `close_dir` of an fd.
    #[tracing::instrument(level = Level::TRACE, skip(self))]
    pub(crate) fn close_dir(&mut self, fd: u64) -> Option<FileResponse> {
        if self.dir_streams.remove(&fd).is_none() && self.getdents_streams.remove(&fd).is_none() {
            error!("FileManager::close_dir -> fd {:#?} not found", fd);
        } else {
            OPEN_FD_COUNT.dec();
        }

        None
    }

    pub(crate) fn access(
        &mut self,
        pathname: PathBuf,
        mode: u8,
    ) -> RemoteResult<AccessFileResponse> {
        let pathname = resolve_path(pathname, &self.root_path)?;
        trace!(
            "FileManager::access -> pathname {:#?} | mode {:#?}",
            pathname,
            mode,
        );

        // Mirror bit representation of flags to support how the flags are represented in the
        // faccess library
        let mode =
            AccessMode::from_bits((mode << 4).reverse_bits() | 1).unwrap_or(AccessMode::EXISTS);

        pathname
            .access(mode)
            .map(|_| AccessFileResponse)
            .map_err(ResponseError::from)
    }

    #[tracing::instrument(level = "trace", skip(self))]
    pub(crate) fn xstat(
        &mut self,
        path: Option<PathBuf>,
        fd: Option<u64>,
        follow_symlink: bool,
    ) -> RemoteResult<XstatResponse> {
        let path = match (path, fd) {
            // lstat/stat or fstatat with fdcwd
            (Some(path), None) => path,
            // fstatat
            (Some(path), Some(fd)) => {
                if let RemoteFile::Directory(parent_path) = self
                    .open_files
                    .get(&fd)
                    .ok_or(ResponseError::NotFound(fd))?
                {
                    parent_path.join(path)
                } else {
                    return Err(ResponseError::NotDirectory(fd));
                }
            }
            // fstat
            (None, Some(fd)) => {
                match self
                    .open_files
                    .get(&fd)
                    .ok_or(ResponseError::NotFound(fd))?
                {
                    RemoteFile::File(file) => {
                        return Ok(XstatResponse {
                            metadata: file.metadata()?.into(),
                        })
                    }
                    RemoteFile::Directory(path) => {
                        return Ok(XstatResponse {
                            metadata: path.metadata()?.into(),
                        })
                    }
                }
            }
            // invalid
            _ => return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput).into()),
        };
        let path = path.strip_prefix("/").map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "couldn't strip prefix")
        })?;
        let res = if follow_symlink {
            resolve_path(path, &self.root_path)?.metadata()
        } else {
            self.root_path.join(path).symlink_metadata()
        };

        res.map(|metadata| XstatResponse {
            metadata: metadata.into(),
        })
        .map_err(ResponseError::from)
    }

    #[tracing::instrument(level = "trace", skip(self))]
    pub(crate) fn xstatfs(&mut self, fd: u64) -> RemoteResult<XstatFsResponse> {
        let target = self
            .open_files
            .get(&fd)
            .ok_or(ResponseError::NotFound(fd))?;

        let statfs = match target {
            RemoteFile::File(file) => nix::sys::statfs::fstatfs(file)
                .map_err(|err| std::io::Error::from_raw_os_error(err as i32))?,
            RemoteFile::Directory(path) => nix::sys::statfs::statfs(path)
                .map_err(|err| std::io::Error::from_raw_os_error(err as i32))?,
        };

        Ok(XstatFsResponse {
            metadata: statfs.into(),
        })
    }

    #[tracing::instrument(level = Level::TRACE, skip(self), err(level = Level::DEBUG))]
    pub(crate) fn fdopen_dir(&mut self, fd: u64) -> RemoteResult<OpenDirResponse> {
        let path = match self
            .open_files
            .get(&fd)
            .ok_or(ResponseError::NotFound(fd))?
        {
            RemoteFile::Directory(path) => Ok(path),
            _ => Err(ResponseError::NotDirectory(fd)),
        }?;

        let fd = self
            .fds_iter
            .next()
            .ok_or_else(|| ResponseError::IdsExhausted("fdopen_dir".to_string()))?;

        let dir_stream = path.read_dir()?.enumerate();

        if self.dir_streams.insert(fd, dir_stream).is_none() {
            OPEN_FD_COUNT.inc();
        }

        Ok(OpenDirResponse { fd })
    }

    #[tracing::instrument(level = "trace", skip(self))]
    pub(crate) fn get_dir_stream(&mut self, fd: u64) -> RemoteResult<&mut Enumerate<ReadDir>> {
        self.dir_streams
            .get_mut(&fd)
            .ok_or(ResponseError::NotFound(fd))
    }

    fn path_to_dir_entry_internal(
        path: &Path,
        position: u64,
        name: String,
    ) -> io::Result<DirEntryInternal> {
        let metadata = std::fs::metadata(path)?;
        Ok(DirEntryInternal {
            inode: metadata.ino(),
            position,
            name,
            file_type: DT_DIR,
        })
    }

    /// Get an iterator that contains entries of the current and parent (if exists) directories,
    /// to chain with the iterator returned by [`std::fs::read_dir`].
    fn get_current_and_parent_entries(current: &Path) -> VecDeque<io::Result<DirEntryInternal>> {
        let mut entries = VecDeque::default();
        entries.push_back(Self::path_to_dir_entry_internal(
            current,
            0,
            ".".to_string(),
        ));
        if let Some(parent) = current.parent() {
            entries.push_back(Self::path_to_dir_entry_internal(
                parent,
                1,
                "..".to_string(),
            ))
        }
        entries
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
                Some(RemoteFile::Directory(dir)) => {
                    let current_and_parent = Self::get_current_and_parent_entries(dir);
                    let stream =
                        GetDEnts64Stream::new(dir.read_dir()?, current_and_parent).peekable();
                    // TODO(alex) [mid]: Do we also want to count streams of stuffs?
                    Ok(e.insert(stream))
                }
            },
            Entry::Occupied(existing) => Ok(existing.into_mut()),
        }
    }

    #[tracing::instrument(level = Level::TRACE, skip(self), ret)]
    pub(crate) fn read_dir(&mut self, fd: u64) -> RemoteResult<ReadDirResponse> {
        let dir_stream = self.get_dir_stream(fd)?;
        let result = if let Some(offset_entry_pair) = dir_stream.next() {
            ReadDirResponse {
                direntry: Some(offset_entry_pair.try_into()?),
            }
        } else {
            ReadDirResponse { direntry: None }
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
            .try_collect::<Vec<_>>()
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
                .next_if(|entry_res: &AgentResult<DirEntryInternal, io::Error>| {
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
