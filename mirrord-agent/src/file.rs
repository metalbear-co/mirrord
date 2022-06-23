use std::{
    self,
    collections::HashMap,
    fs::{File, OpenOptions},
    io::{prelude::*, SeekFrom},
    path::PathBuf,
};

use mirrord_protocol::{
    CloseFileRequest, CloseFileResponse, ErrorKindInternal, FileError, FileRequest, FileResponse,
    OpenFileRequest, OpenFileResponse, OpenOptionsInternal, OpenRelativeFileRequest,
    ReadFileRequest, ReadFileResponse, ResponseError, SeekFileRequest, SeekFileResponse,
    WriteFileRequest, WriteFileResponse,
};
use tracing::{debug, error};

use crate::{
    error::AgentError,
    util::{IndexAllocator, ReusableIndex},
};

#[derive(Debug)]
pub enum RemoteFile {
    File(File),
    Directory(PathBuf),
}

#[derive(Debug, Default)]
pub struct FileManager {
    root_path: PathBuf,
    pub open_files: HashMap<ReusableIndex<usize>, RemoteFile>,
    index_allocator: IndexAllocator<usize>,
}

impl FileManager {
    /// Executes the request and returns the response.
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
            FileRequest::Read(ReadFileRequest { fd, buffer_size }) => {
                let read_result = self.read(fd, buffer_size);
                Ok(FileResponse::Read(read_result))
            }
            FileRequest::Seek(SeekFileRequest { fd, seek_from }) => {
                let seek_result = self.seek(fd, seek_from.into());
                Ok(FileResponse::Seek(seek_result))
            }
            FileRequest::Write(WriteFileRequest { fd, write_bytes }) => {
                let write_result = self.write(fd, write_bytes);
                Ok(FileResponse::Write(write_result))
            }
            FileRequest::Close(CloseFileRequest { fd }) => {
                let close_result = self.close(fd);
                Ok(FileResponse::Close(close_result))
            }
        }
    }

    pub fn new(pid: Option<u64>) -> Self {
        let root_path = match pid {
            Some(pid) => PathBuf::from("/proc").join(pid.to_string()).join("root"),
            None => PathBuf::from("/"),
        };
        Self {
            open_files: HashMap::new(),
            root_path,
            ..Default::default()
        }
    }
    fn open(
        &mut self,
        path: PathBuf,
        open_options: OpenOptionsInternal,
    ) -> Result<OpenFileResponse, ResponseError> {
        debug!(
            "FileManager::open -> Trying to open file {:#?} | options {:#?}",
            path, open_options
        );

        let file = OpenOptions::from(open_options)
            .open(&path)
            .map_err(|fail| {
                error!(
                    "FileManager::open -> Failed to open file {:#?} | error {:#?}",
                    path, fail
                );
                ResponseError::FileOperation(FileError {
                    operation: "open".to_string(),
                    raw_os_error: fail.raw_os_error(),
                    kind: fail.kind().into(),
                })
            })?;

        let fd = self.index_allocator.next_index().ok_or_else(|| {
            error!("FileManager::open -> Failed to allocate file descriptor");
            ResponseError::FileOperation(FileError {
                operation: "open".to_string(),
                raw_os_error: Some(-1),
                kind: ErrorKindInternal::Other,
            })
        })?;

        match file.metadata() {
            Ok(metadata) => {
                debug!("FileManager::open -> Got metadata for file {:#?}", metadata);
                let remote_file = if metadata.is_dir() {
                    RemoteFile::Directory(path)
                } else {
                    RemoteFile::File(file)
                };
                let res = Ok(OpenFileResponse { fd: *fd });
                self.open_files.insert(fd, remote_file);
                res
            }
            Err(err) => {
                error!(
                    "FileManager::open_relative -> Failed to get metadata for file {:#?}",
                    err
                );
                Err(ResponseError::FileOperation(FileError {
                    operation: "open".to_string(),
                    raw_os_error: err.raw_os_error(),
                    kind: err.kind().into(),
                }))
            }
        }
    }

    fn open_relative(
        &mut self,
        relative_fd: usize,
        path: PathBuf,
        open_options: OpenOptionsInternal,
    ) -> Result<OpenFileResponse, ResponseError> {
        debug!(
            "FileManager::open_relative -> Trying to open {:#?} | options {:#?} | fd {:#?}",
            path, open_options, relative_fd
        );

        let relative_dir = self
            .open_files
            .get(&relative_fd)
            .ok_or(ResponseError::NotFound)?;

        if let RemoteFile::Directory(relative_dir) = relative_dir {
            let path = relative_dir.join(&path);
            debug!(
                "FileManager::open_relative -> Trying to open complete path: {:#?}",
                path
            );
            let file = OpenOptions::from(open_options)
                .open(&path)
                .map_err(|fail| {
                    error!(
                        "FileManager::open -> Failed to open file {:#?} | error {:#?}",
                        path, fail
                    );
                    ResponseError::FileOperation(FileError {
                        operation: "open".to_string(),
                        raw_os_error: fail.raw_os_error(),
                        kind: fail.kind().into(),
                    })
                })?;

            let fd = self.index_allocator.next_index().ok_or_else(|| {
                error!("FileManager::open -> Failed to allocate file descriptor");
                ResponseError::FileOperation(FileError {
                    operation: "open".to_string(),
                    raw_os_error: Some(-1),
                    kind: ErrorKindInternal::Other,
                })
            })?;

            match file.metadata() {
                Ok(metadata) => {
                    debug!("FileManager::open -> Got metadata for file {:#?}", metadata);
                    let remote_file = if metadata.is_dir() {
                        RemoteFile::Directory(path)
                    } else {
                        RemoteFile::File(file)
                    };
                    let res = Ok(OpenFileResponse { fd: *fd });
                    self.open_files.insert(fd, remote_file);
                    res
                }
                Err(err) => {
                    error!(
                        "FileManager::open_relative -> Failed to get metadata for file {:#?}",
                        err
                    );
                    Err(ResponseError::FileOperation(FileError {
                        operation: "open".to_string(),
                        raw_os_error: err.raw_os_error(),
                        kind: err.kind().into(),
                    }))
                }
            }
        } else {
            Err(ResponseError::NotFound)
        }
    }

    pub(crate) fn read(
        &mut self,
        fd: usize,
        buffer_size: usize,
    ) -> Result<ReadFileResponse, ResponseError> {
        let remote_file = self
            .open_files
            .get_mut(&fd)
            .ok_or(ResponseError::NotFound)?;

        if let RemoteFile::File(file) = remote_file {
            debug!(
                "FileManager::read -> Trying to read file {:#?}, with count {:#?}",
                file, buffer_size
            );

            let mut buffer = vec![0; buffer_size];
            file.read(&mut buffer).map(|read_amount| {
                debug!(
                    "FileManager::read -> read {:#?} bytes from fd {:#?}",
                    read_amount, fd
                );

                ReadFileResponse {
                    bytes: buffer,
                    read_amount,
                }
            })
        } else {
            return Err(ResponseError::NotFound);
        }
        .map_err(|fail| {
            ResponseError::FileOperation(FileError {
                operation: "read".to_string(),
                raw_os_error: fail.raw_os_error(),
                kind: fail.kind().into(),
            })
        })
    }

    pub(crate) fn seek(
        &mut self,
        fd: usize,
        seek_from: SeekFrom,
    ) -> Result<SeekFileResponse, ResponseError> {
        let file = self
            .open_files
            .get_mut(&fd)
            .ok_or(ResponseError::NotFound)?;

        if let RemoteFile::File(file) = file {
            debug!(
                "FileManager::seek -> Trying to seek file {:#?}, with seek {:#?}",
                file, seek_from
            );

            file.seek(seek_from)
                .map(|result_offset| SeekFileResponse { result_offset })
        } else {
            return Err(ResponseError::NotFound);
        }
        .map_err(|fail| {
            ResponseError::FileOperation(FileError {
                operation: "seek".to_string(),
                raw_os_error: fail.raw_os_error(),
                kind: fail.kind().into(),
            })
        })
    }

    pub(crate) fn write(
        &mut self,
        fd: usize,
        write_bytes: Vec<u8>,
    ) -> Result<WriteFileResponse, ResponseError> {
        let file = self
            .open_files
            .get_mut(&fd)
            .ok_or(ResponseError::NotFound)?;

        if let RemoteFile::File(file) = file {
            debug!(
                "FileManager::write -> Trying to write file {:#?}, with bytes {:#?}",
                file, write_bytes
            );

            file.write(&write_bytes)
                .map(|write_amount| {
                    debug!(
                        "FileManager::write -> wrote {:#?} bytes to fd {:#?}",
                        write_amount, fd
                    );

                    WriteFileResponse {
                        written_amount: write_amount,
                    }
                })
                .map_err(|fail| {
                    ResponseError::FileOperation(FileError {
                        operation: "write".to_string(),
                        raw_os_error: fail.raw_os_error(),
                        kind: fail.kind().into(),
                    })
                })
        } else {
            Err(ResponseError::NotFound)
        }
    }

    pub(crate) fn close(&mut self, fd: usize) -> Result<CloseFileResponse, ResponseError> {
        let file = self.open_files.remove(&fd).ok_or(ResponseError::NotFound)?;
        self.index_allocator.free_index(fd);
        debug!("FileManager::write -> Trying to close file {:#?}", file);

        Ok(CloseFileResponse)
    }
}
