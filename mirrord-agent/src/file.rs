use std::{
    self,
    collections::HashMap,
    fs::{File, OpenOptions},
    io::{prelude::*, BufReader, SeekFrom},
    os::unix::prelude::FileExt,
    path::PathBuf,
};

use faccess::{AccessMode, PathExt};
use mirrord_protocol::{
    AccessFileRequest, AccessFileResponse, CloseFileRequest, CloseFileResponse, FileRequest,
    FileResponse, OpenFileRequest, OpenFileResponse, OpenOptionsInternal, OpenRelativeFileRequest,
    ReadFileRequest, ReadFileResponse, ReadLimitedFileRequest, ReadLineFileRequest, RemoteResult,
    ResponseError, SeekFileRequest, SeekFileResponse, WriteFileRequest, WriteFileResponse,
    WriteLimitedFileRequest,
};
use tracing::{debug, error, trace};

use crate::{error::AgentError, util::IndexAllocator};

#[derive(Debug)]
pub enum RemoteFile {
    File(File),
    Directory(PathBuf),
}

#[derive(Debug, Default)]
pub struct FileManager {
    root_path: PathBuf,
    pub open_files: HashMap<usize, RemoteFile>,
    index_allocator: IndexAllocator<usize>,
}

impl FileManager {
    /// Executes the request and returns the response.
    #[tracing::instrument(level = "trace", skip(self))]
    pub fn handle_message(&mut self, request: FileRequest) -> Result<FileResponse, AgentError> {
        let root_path = &self.root_path;
        match request {
            FileRequest::Open(OpenFileRequest { path, open_options }) => {
                // TODO: maybe not agent error on this?
                let path = path
                    .strip_prefix("/")
                    .inspect_err(|fail| error!("file_worker -> {:#?}", fail))?;

                // Should be something like `/proc/{pid}/root/{path}`
                let full_path = root_path.as_path().join(path);

                let open_result = self.open(full_path, open_options);
                Ok(FileResponse::Open(open_result))
            }
            FileRequest::OpenRelative(OpenRelativeFileRequest {
                relative_fd,
                path,
                open_options,
            }) => {
                let open_result = self.open_relative(relative_fd, path, open_options);
                Ok(FileResponse::Open(open_result))
            }
            FileRequest::Read(ReadFileRequest {
                remote_fd,
                buffer_size,
            }) => {
                let read_result = self.read(remote_fd, buffer_size);
                Ok(FileResponse::Read(read_result))
            }
            FileRequest::ReadLine(ReadLineFileRequest {
                remote_fd,
                buffer_size,
            }) => {
                let read_result = self.read_line(remote_fd, buffer_size);
                Ok(FileResponse::ReadLine(read_result))
            }
            FileRequest::ReadLimited(ReadLimitedFileRequest {
                remote_fd,
                buffer_size,
                start_from,
            }) => Ok(FileResponse::ReadLimited(self.read_limited(
                remote_fd,
                buffer_size,
                start_from,
            ))),
            FileRequest::Seek(SeekFileRequest { fd, seek_from }) => {
                let seek_result = self.seek(fd, seek_from.into());
                Ok(FileResponse::Seek(seek_result))
            }
            FileRequest::Write(WriteFileRequest { fd, write_bytes }) => {
                let write_result = self.write(fd, write_bytes);
                Ok(FileResponse::Write(write_result))
            }
            FileRequest::WriteLimited(WriteLimitedFileRequest {
                remote_fd,
                start_from,
                write_bytes,
            }) => {
                let write_result = self.write_limited(remote_fd, start_from, write_bytes);
                Ok(FileResponse::WriteLimited(write_result))
            }
            FileRequest::Close(CloseFileRequest { fd }) => {
                let close_result = self.close(fd);
                Ok(FileResponse::Close(close_result))
            }
            FileRequest::Access(AccessFileRequest { pathname, mode }) => {
                let pathname = pathname
                    .strip_prefix("/")
                    .inspect_err(|fail| error!("file_worker -> {:#?}", fail))?;

                // Should be something like `/proc/{pid}/root/{path}`
                let full_path = root_path.as_path().join(pathname);

                let access_result = self.access(full_path, mode);
                Ok(FileResponse::Access(access_result))
            }
        }
    }

    #[tracing::instrument(level = "trace")]
    pub fn new(pid: Option<u64>) -> Self {
        let root_path = match pid {
            Some(pid) => PathBuf::from("/proc").join(pid.to_string()).join("root"),
            None => PathBuf::from("/"),
        };
        debug!("Agent root path >> {root_path:?}");
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
        relative_fd: usize,
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
    pub(crate) fn read(&mut self, fd: usize, buffer_size: usize) -> RemoteResult<ReadFileResponse> {
        self.open_files
            .get_mut(&fd)
            .ok_or(ResponseError::NotFound(fd))
            .and_then(|remote_file| {
                if let RemoteFile::File(file) = remote_file {
                    let mut buffer = vec![0; buffer_size];
                    let read_amount =
                        file.read(&mut buffer).map(|read_amount| ReadFileResponse {
                            bytes: buffer,
                            read_amount,
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
        fd: usize,
        buffer_size: usize,
    ) -> RemoteResult<ReadFileResponse> {
        self.open_files
            .get_mut(&fd)
            .ok_or(ResponseError::NotFound(fd))
            .and_then(|remote_file| {
                if let RemoteFile::File(file) = remote_file {
                    let mut reader = BufReader::new(std::io::Read::by_ref(file));
                    let mut buffer = String::with_capacity(buffer_size);
                    let read_result = reader
                        .read_line(&mut buffer)
                        .and_then(|read_amount| {
                            // Take the new position to update the file's cursor position later.
                            let position_after_read = reader.stream_position()?;

                            // Limit the new position to `buffer_size`.
                            Ok((
                                read_amount,
                                position_after_read.clamp(0, buffer_size as u64),
                            ))
                        })
                        .and_then(|(read_amount, seek_to)| {
                            file.seek(SeekFrom::Start(seek_to))?;

                            // We handle the extra bytes in the `fgets` hook, so here we can just
                            // return the full buffer.
                            let response = ReadFileResponse {
                                bytes: buffer.into_bytes(),
                                read_amount,
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
        fd: usize,
        buffer_size: usize,
        start_from: u64,
    ) -> RemoteResult<ReadFileResponse> {
        self.open_files
            .get_mut(&fd)
            .ok_or(ResponseError::NotFound(fd))
            .and_then(|remote_file| {
                if let RemoteFile::File(file) = remote_file {
                    let mut buffer = vec![0; buffer_size];

                    let read_result = file.read_at(&mut buffer, start_from).map(|read_amount| {
                        // We handle the extra bytes in the `pread` hook, so here we can just
                        // return the full buffer.
                        ReadFileResponse {
                            bytes: buffer,
                            read_amount,
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
        fd: usize,
        start_from: u64,
        buffer: Vec<u8>,
    ) -> RemoteResult<WriteFileResponse> {
        self.open_files
            .get_mut(&fd)
            .ok_or(ResponseError::NotFound(fd))
            .and_then(|remote_file| {
                if let RemoteFile::File(file) = remote_file {
                    let written_amount = file
                        .write_at(&buffer, start_from)
                        .map(|written_amount| WriteFileResponse { written_amount })?;

                    Ok(written_amount)
                } else {
                    Err(ResponseError::NotFile(fd))
                }
            })
    }

    pub(crate) fn seek(
        &mut self,
        fd: usize,
        seek_from: SeekFrom,
    ) -> RemoteResult<SeekFileResponse> {
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
        fd: usize,
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
                                written_amount: write_amount,
                            })?;

                    Ok(write_result)
                } else {
                    Err(ResponseError::NotFile(fd))
                }
            })
    }

    pub(crate) fn close(&mut self, fd: usize) -> RemoteResult<CloseFileResponse> {
        trace!("FileManager::close -> fd {:#?}", fd,);

        let _file = self
            .open_files
            .remove(&fd)
            .ok_or(ResponseError::NotFound(fd))?;

        self.index_allocator.free_index(fd);

        Ok(CloseFileResponse)
    }

    pub(crate) fn access(
        &mut self,
        pathname: PathBuf,
        mode: u8,
    ) -> RemoteResult<AccessFileResponse> {
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
}
