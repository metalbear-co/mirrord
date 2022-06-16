use std::{
    self,
    collections::HashMap,
    fs::{File, OpenOptions},
    io::{prelude::*, SeekFrom},
    path::PathBuf,
};

use mirrord_protocol::{
    CloseFileRequest, CloseFileResponse, FileError, FileRequest, FileResponse, OpenFileRequest,
    OpenFileResponse, OpenOptionsInternal, OpenRelativeFileRequest, ReadFileRequest,
    ReadFileResponse, ResponseError, SeekFileRequest, SeekFileResponse, WriteFileRequest,
    WriteFileResponse,
};
use tracing::{debug, error};

use crate::error::AgentError;

// TODO: To help with `openat`, instead of `HashMap<_, File>` this should be
// `HashMap<_, RemoteFile`>, where `RemoteFile` is a struct that holds both the `File` + `PathBuf`.
#[derive(Debug, Default)]
pub struct FileManager {
    open_files: HashMap<i32, File>,
    root_path: PathBuf,
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
                let path = path
                    .strip_prefix("/")
                    .inspect_err(|fail| error!("file_worker -> {:#?}", fail))?;

                // Should be something like `/proc/{pid}/root/{path}`
                let full_path = root_path.as_path().join(path);

                let open_result = self.open_relative(relative_fd, full_path, open_options);
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

        OpenOptions::from(open_options)
            .open(path)
            .map(|file| {
                let fd = std::os::unix::prelude::AsRawFd::as_raw_fd(&file);
                self.open_files.insert(fd, file);

                OpenFileResponse { fd }
            })
            .map_err(|fail| {
                ResponseError::FileOperation(FileError {
                    operation: "open".to_string(),
                    raw_os_error: fail.raw_os_error(),
                    kind: fail.kind().into(),
                })
            })
    }

    fn open_relative(
        &mut self,
        relative_fd: i32,
        path: PathBuf,
        open_options: OpenOptionsInternal,
    ) -> Result<OpenFileResponse, ResponseError> {
        debug!(
            "FileManager::open_relative -> Trying to open {:#?} | options {:#?} | fd {:#?}",
            path, open_options, relative_fd
        );

        let _relative_dir = self
            .open_files
            .get(&relative_fd)
            .ok_or(ResponseError::NotFound)?;

        OpenOptions::from(open_options)
            .open(path)
            .map(|file| {
                let fd = std::os::unix::prelude::AsRawFd::as_raw_fd(&file);
                self.open_files.insert(fd, file);

                OpenFileResponse { fd }
            })
            .map_err(|fail| {
                ResponseError::FileOperation(FileError {
                    operation: "open".to_string(),
                    raw_os_error: fail.raw_os_error(),
                    kind: fail.kind().into(),
                })
            })
    }

    fn read(&mut self, fd: i32, buffer_size: usize) -> Result<ReadFileResponse, ResponseError> {
        let file = self
            .open_files
            .get_mut(&fd)
            .ok_or(ResponseError::NotFound)?;

        debug!(
            "FileManager::read -> Trying to read file {:#?}, with count {:#?}",
            file, buffer_size
        );

        let mut buffer = vec![0; buffer_size];
        file.read(&mut buffer)
            .map(|read_amount| {
                debug!(
                    "FileManager::read -> read {:#?} bytes from fd {:#?}",
                    read_amount, fd
                );

                ReadFileResponse {
                    bytes: buffer,
                    read_amount,
                }
            })
            .map_err(|fail| {
                ResponseError::FileOperation(FileError {
                    operation: "read".to_string(),
                    raw_os_error: fail.raw_os_error(),
                    kind: fail.kind().into(),
                })
            })
    }

    fn seek(&mut self, fd: i32, seek_from: SeekFrom) -> Result<SeekFileResponse, ResponseError> {
        let file = self
            .open_files
            .get_mut(&fd)
            .ok_or(ResponseError::NotFound)?;

        debug!(
            "FileManager::seek -> Trying to seek file {:#?}, with seek {:#?}",
            file, seek_from
        );

        file.seek(seek_from)
            .map(|result_offset| SeekFileResponse { result_offset })
            .map_err(|fail| {
                ResponseError::FileOperation(FileError {
                    operation: "seek".to_string(),
                    raw_os_error: fail.raw_os_error(),
                    kind: fail.kind().into(),
                })
            })
    }

    fn write(&mut self, fd: i32, write_bytes: Vec<u8>) -> Result<WriteFileResponse, ResponseError> {
        let file = self
            .open_files
            .get_mut(&fd)
            .ok_or(ResponseError::NotFound)?;

        debug!("FileManager::write -> Trying to write file {:#?}", file);

        file.write(&write_bytes)
            .map(|written_amount| WriteFileResponse { written_amount })
            .map_err(|fail| {
                ResponseError::FileOperation(FileError {
                    operation: "write".to_string(),
                    raw_os_error: fail.raw_os_error(),
                    kind: fail.kind().into(),
                })
            })
    }

    fn close(&mut self, fd: i32) -> Result<CloseFileResponse, ResponseError> {
        let file = self.open_files.remove(&fd).ok_or(ResponseError::NotFound)?;

        debug!("FileManager::write -> Trying to close file {:#?}", file);

        Ok(CloseFileResponse)
    }
}
