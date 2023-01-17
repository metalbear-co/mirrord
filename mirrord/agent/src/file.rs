use std::{
    self,
    collections::HashMap,
    fs::{File, OpenOptions, ReadDir},
    io,
    io::{prelude::*, BufReader, SeekFrom},
    iter::Enumerate,
    os::unix::prelude::FileExt,
    path::{Path, PathBuf},
};

use faccess::{AccessMode, PathExt};
#[cfg(target_os = "linux")]
use mirrord_protocol::file::{Getdents64Request, Getdents64Response};
use mirrord_protocol::{
    file::{
        AccessFileRequest, AccessFileResponse, CloseDirRequest, CloseFileRequest, DirEntryInternal,
        FdOpenDirRequest, OpenDirResponse, OpenFileRequest, OpenFileResponse, OpenOptionsInternal,
        OpenRelativeFileRequest, ReadDirRequest, ReadDirResponse, ReadFileRequest,
        ReadFileResponse, ReadLimitedFileRequest, ReadLineFileRequest, SeekFileRequest,
        SeekFileResponse, WriteFileRequest, WriteFileResponse, WriteLimitedFileRequest,
        XstatRequest, XstatResponse,
    },
    FileRequest, FileResponse, RemoteResult, ResponseError,
};
use tracing::{error, trace};

use crate::{error::Result, util::IndexAllocator};

#[derive(Debug)]
pub enum RemoteFile {
    File(File),
    Directory(PathBuf),
}

#[derive(Debug, Default)]
pub struct FileManager {
    root_path: PathBuf,
    pub open_files: HashMap<u64, RemoteFile>,
    pub dir_streams: HashMap<u64, Enumerate<ReadDir>>,
    index_allocator: IndexAllocator<u64>,
}

/// Resolve a path that might contain symlinks from a specific container to a path accessible from
/// the root host
#[tracing::instrument(level = "trace")]
fn resolve_path<P: AsRef<Path> + std::fmt::Debug, R: AsRef<Path> + std::fmt::Debug>(
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
    #[tracing::instrument(level = "trace", skip(self))]
    pub fn handle_message(&mut self, request: FileRequest) -> Result<Option<FileResponse>> {
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
            FileRequest::ReadLine(ReadLineFileRequest {
                remote_fd,
                buffer_size,
            }) => {
                let read_result = self.read_line(remote_fd, buffer_size);
                Some(FileResponse::ReadLine(read_result))
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
            FileRequest::Close(CloseFileRequest { fd }) => {
                self.close(fd);
                None
            }
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

            FileRequest::FdOpenDir(FdOpenDirRequest { remote_fd }) => {
                let open_dir_result = self.fdopen_dir(remote_fd);
                Some(FileResponse::OpenDir(open_dir_result))
            }

            FileRequest::ReadDir(ReadDirRequest { remote_fd }) => {
                let read_dir_result = self.read_dir(remote_fd);
                Some(FileResponse::ReadDir(read_dir_result))
            }
            FileRequest::CloseDir(CloseDirRequest { remote_fd }) => {
                self.close_dir(remote_fd);
                None
            }
            #[cfg(target_os = "linux")]
            FileRequest::Getdents64(Getdents64Request {
                remote_fd,
                buffer_size,
            }) => Some(FileResponse::Getdents64(
                self.getdents64(remote_fd, buffer_size),
            )),
        })
    }

    #[tracing::instrument(level = "trace")]
    pub fn new(pid: Option<u64>) -> Self {
        let root_path = match pid {
            Some(pid) => PathBuf::from("/proc").join(pid.to_string()).join("root"),
            None => PathBuf::from("/"),
        };
        trace!("Agent root path >> {root_path:?}");
        Self {
            open_files: HashMap::new(),
            root_path,
            ..Default::default()
        }
    }

    #[tracing::instrument(level = "trace", skip(self))]
    fn open(
        &mut self,
        path: PathBuf,
        open_options: OpenOptionsInternal,
    ) -> RemoteResult<OpenFileResponse> {
        let path = resolve_path(path, &self.root_path)?;
        let file = OpenOptions::from(open_options).open(&path)?;

        let fd = self
            .index_allocator
            .next_index()
            .ok_or_else(|| ResponseError::AllocationFailure("FileManager::open".to_string()))?;

        let metadata = file.metadata()?;

        let remote_file = if metadata.is_dir() {
            RemoteFile::Directory(path)
        } else {
            RemoteFile::File(file)
        };

        self.open_files.insert(fd, remote_file);

        Ok(OpenFileResponse { fd })
    }

    #[tracing::instrument(level = "trace", skip(self))]
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

            let fd = self.index_allocator.next_index().ok_or_else(|| {
                ResponseError::AllocationFailure("FileManager::open_relative".to_string())
            })?;

            let metadata = file.metadata()?;

            let remote_file = if metadata.is_dir() {
                RemoteFile::Directory(path)
            } else {
                RemoteFile::File(file)
            };

            self.open_files.insert(fd, remote_file);

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
                    let read_amount =
                        file.read(&mut buffer).map(|read_amount| ReadFileResponse {
                            bytes: buffer,
                            read_amount: read_amount as u64,
                        })?;

                    Ok(read_amount)
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
                    let mut reader = BufReader::new(std::io::Read::by_ref(file));
                    let mut buffer = String::with_capacity(buffer_size as usize);
                    let read_result = reader
                        .read_line(&mut buffer)
                        .and_then(|read_amount| {
                            // Take the new position to update the file's cursor position later.
                            let position_after_read = reader.stream_position()?;

                            // Limit the new position to `buffer_size`.
                            Ok((read_amount, position_after_read.clamp(0, buffer_size)))
                        })
                        .and_then(|(read_amount, seek_to)| {
                            file.seek(SeekFrom::Start(seek_to))?;

                            // We handle the extra bytes in the `fgets` hook, so here we can just
                            // return the full buffer.
                            let response = ReadFileResponse {
                                bytes: buffer.into_bytes(),
                                read_amount: read_amount as u64,
                            };

                            Ok(response)
                        })?;

                    Ok(read_result)
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

                    let read_result = file.read_at(&mut buffer, start_from).map(|read_amount| {
                        // We handle the extra bytes in the `pread` hook, so here we can just
                        // return the full buffer.
                        ReadFileResponse {
                            bytes: buffer,
                            read_amount: read_amount as u64,
                        }
                    })?;

                    Ok(read_result)
                } else {
                    Err(ResponseError::NotFile(fd))
                }
            })
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

    pub(crate) fn close(&mut self, fd: u64) {
        trace!("FileManager::close -> fd {:#?}", fd,);

        if self.open_files.remove(&fd).is_none() {
            error!("FileManager::close -> fd {:#?} not found", fd);
        } else {
            self.index_allocator.free_index(fd);
        }
    }

    pub(crate) fn close_dir(&mut self, fd: u64) {
        trace!("FileManager::close_dir -> fd {:#?}", fd,);

        if self.dir_streams.remove(&fd).is_none() {
            error!("FileManager::close_dir -> fd {:#?} not found", fd);
        } else {
            self.index_allocator.free_index(fd);
        }
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
    pub(crate) fn fdopen_dir(&mut self, fd: u64) -> RemoteResult<OpenDirResponse> {
        let path = match self
            .open_files
            .get(&fd)
            .ok_or(ResponseError::NotFound(fd))?
        {
            RemoteFile::Directory(path) => Ok(path),
            _ => Err(ResponseError::NotDirectory(fd)),
        }?;

        let fd = self.index_allocator.next_index().ok_or_else(|| {
            ResponseError::AllocationFailure("FileManager::fdopen_dir".to_string())
        })?;

        let dir_stream = path.read_dir()?.enumerate();
        self.dir_streams.insert(fd, dir_stream);

        Ok(OpenDirResponse { fd })
    }

    #[tracing::instrument(level = "trace", skip(self))]
    pub(crate) fn get_dir_stream(&mut self, fd: u64) -> RemoteResult<&mut Enumerate<ReadDir>> {
        self.dir_streams
            .get_mut(&fd)
            .ok_or(ResponseError::NotFound(fd))
    }

    #[tracing::instrument(level = "trace", skip(self))]
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

    #[cfg(target_os = "linux")]
    #[tracing::instrument(level = "trace", skip(self))]
    pub(crate) fn getdents64(
        &mut self,
        fd: u64,
        buffer_size: u64,
    ) -> RemoteResult<Getdents64Response> {
        let mut result_size = 0u64;

        let log_err = |entry_res: Result<DirEntryInternal, io::Error>| {
            entry_res.inspect_err(|err| error!("Converting DirEntry failed with {err:?}"))
        };

        let mut entry_results = self
            .get_dir_stream(fd)?
            .map(TryInto::try_into) // Convert into DirEntryInternal.
            .map(log_err)
            .peekable();

        // Trying to allocate according to what the syscall caller allocated.
        // The caller of the syscall allocated buffer_size bytes, so if the average linux_dirent64
        // in this dir is not bigger than 32 this should be enough.
        // But don't preallocate more than 256.
        let initial_vector_capacity = 256.min((buffer_size / 32) as usize);
        let mut entries = Vec::with_capacity(initial_vector_capacity);

        // Peek into the next result, and only consume it if there is room for it in the buffer (and
        // there was no error converting to a `DirEntryInternal`.
        while let Some(entry) = entry_results
            .next_if(|entry_res: &Result<DirEntryInternal, io::Error>| {
                entry_res
                    .as_ref()
                    .is_ok_and(|entry| entry.get_d_reclen64() as u64 + result_size <= buffer_size)
            })
            .transpose()?
        {
            result_size += entry.get_d_reclen64() as u64;
            entries.push(entry);
        }

        Ok(Getdents64Response {
            fd,
            entries,
            result_size,
        })
    }
}
