use std::{
    self,
    collections::HashMap,
    fs::{File, OpenOptions},
    io::{prelude::*, SeekFrom},
    path::PathBuf,
};

use mirrord_protocol::{
    CloseFileRequest, CloseFileResponse, FileRequest, FileResponse, OpenFileRequest,
    OpenFileResponse, OpenOptionsInternal, OpenRelativeFileRequest, ReadFileRequest,
    ReadFileResponse, RemoteResult, ResponseError, SeekFileRequest, SeekFileResponse,
    WriteFileRequest, WriteFileResponse,
};
use tracing::{error, trace};

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
    ) -> RemoteResult<OpenFileResponse> {
        trace!(
            "FileManager::open -> path {:#?} | open_options {:#?}",
            path,
            open_options
        );

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

    fn open_relative(
        &mut self,
        relative_fd: usize,
        path: PathBuf,
        open_options: OpenOptionsInternal,
    ) -> RemoteResult<OpenFileResponse> {
        trace!(
            "FileManager::open_relative -> relative_fd {:#?} | path {:#?} | open_options {:#?}",
            relative_fd,
            path,
            open_options,
        );

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

    pub(crate) fn read(&mut self, fd: usize, buffer_size: usize) -> RemoteResult<ReadFileResponse> {
        trace!(
            "FileManager::read -> fd {:#?} | buffer_size {:#?}",
            fd,
            buffer_size
        );

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
}
