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
use tokio::sync::mpsc::{Receiver, Sender};
use tracing::{debug, error};

use crate::{error::AgentError, runtime::get_container_pid, sniffer::DEFAULT_RUNTIME, PeerID};

#[derive(Debug)]
pub enum RemoteFile {
    File(File),
    Directory(PathBuf),
}

#[derive(Debug, Default)]
pub struct FileManager {
    pub open_files: HashMap<i32, RemoteFile>,
}

impl FileManager {
    pub(crate) fn open(
        &mut self,
        path: PathBuf,
        open_options: OpenOptionsInternal,
    ) -> Result<OpenFileResponse, ResponseError> {
        debug!(
            "FileManager::open -> Trying to open file {:#?} | options {:#?}",
            path, open_options
        );

        OpenOptions::from(open_options)
            .open(&path)
            .map(|file| {
                let fd = std::os::unix::prelude::AsRawFd::as_raw_fd(&file);

                let _ = file
                    .metadata()
                    .map_err(|err| {
                        error!(
                            "FileManager::open -> Failed to get metadata for file {:#?}",
                            err
                        );
                        err
                    })
                    .map(|metadata| {
                        debug!("FileManager::open -> Got metadata for file {:#?}", metadata);
                        if metadata.is_dir() {
                            self.open_files.insert(fd, RemoteFile::Directory(path));
                        } else {
                            self.open_files.insert(fd, RemoteFile::File(file));
                        }
                    });

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

    pub(crate) fn open_relative(
        &mut self,
        relative_fd: i32,
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
            OpenOptions::from(open_options).open(&path).map(|file| {
                let fd = std::os::unix::prelude::AsRawFd::as_raw_fd(&file);

                let _ = file
                    .metadata()
                    .map_err(|err| {
                        error!(
                            "FileManager::open_relative -> Failed to get metadata for file {:#?}",
                            err
                        );
                        err
                    })
                    .map(|metadata| {
                        debug!(
                            "FileManager::open_relative -> Got metadata for file {:#?}",
                            metadata
                        );
                        if metadata.is_dir() {
                            self.open_files.insert(fd, RemoteFile::Directory(path));
                        } else {
                            self.open_files.insert(fd, RemoteFile::File(file));
                        }
                    });

                OpenFileResponse { fd }
            })
        } else {
            return Err(ResponseError::NotFound);
        }
        .map_err(|fail| {
            ResponseError::FileOperation(FileError {
                operation: "open".to_string(),
                raw_os_error: fail.raw_os_error(),
                kind: fail.kind().into(),
            })
        })
    }

    pub(crate) fn read(
        &mut self,
        fd: i32,
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
        fd: i32,
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
        fd: i32,
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

            file.write(&write_bytes).map(|write_amount| {
                debug!(
                    "FileManager::write -> wrote {:#?} bytes to fd {:#?}",
                    write_amount, fd
                );

                WriteFileResponse {
                    written_amount: write_amount,
                }
            })
        } else {
            return Err(ResponseError::NotFound);
        }
        .map_err(|fail| {
            ResponseError::FileOperation(FileError {
                operation: "write".to_string(),
                raw_os_error: fail.raw_os_error(),
                kind: fail.kind().into(),
            })
        })
    }

    pub(crate) fn close(&mut self, fd: i32) -> Result<CloseFileResponse, ResponseError> {
        let file = self.open_files.remove(&fd).ok_or(ResponseError::NotFound)?;

        debug!("FileManager::write -> Trying to close file {:#?}", file);

        Ok(CloseFileResponse)
    }
}

pub async fn file_worker(
    mut file_request_rx: Receiver<(PeerID, FileRequest)>,
    file_response_tx: Sender<(PeerID, FileResponse)>,
    container_id: Option<String>,
    container_runtime: Option<String>,
) -> Result<(), AgentError> {
    debug!("file_worker -> Setting namespace");

    let pid = match container_id {
        Some(container_id) => {
            get_container_pid(
                &container_id,
                &container_runtime.unwrap_or_else(|| DEFAULT_RUNTIME.to_string()),
            )
            .await
        }
        None => Err(AgentError::NotFound(format!(
            "file_worker -> Container ID not specified {:#?} for runtime {:#?}!",
            container_id, container_runtime
        ))),
    }?;

    let root_path = PathBuf::from("/proc").join(pid.to_string()).join("root");
    let mut file_manager = FileManager::default();

    while let Some(file_request) = file_request_rx.recv().await {
        match file_request {
            (peer_id, FileRequest::Open(OpenFileRequest { path, open_options })) => {
                let path = path
                    .strip_prefix("/")
                    .inspect_err(|fail| error!("file_worker -> {:#?}", fail))
                    .unwrap();

                // Should be something like `/proc/{pid}/root/{path}`
                let full_path = root_path.as_path().join(path);

                let open_result = file_manager.open(full_path, open_options);
                let response = FileResponse::Open(open_result);

                file_response_tx.send((peer_id, response)).await.unwrap();
            }
            (
                peer_id,
                FileRequest::OpenRelative(OpenRelativeFileRequest {
                    relative_fd,
                    path,
                    open_options,
                }),
            ) => {
                let open_result = file_manager.open_relative(relative_fd, path, open_options);
                let response = FileResponse::Open(open_result);

                file_response_tx.send((peer_id, response)).await.unwrap();
            }
            (peer_id, FileRequest::Read(ReadFileRequest { fd, buffer_size })) => {
                let read_result = file_manager.read(fd, buffer_size);
                let response = FileResponse::Read(read_result);

                file_response_tx
                    .send((peer_id, response))
                    .await
                    .inspect_err(|fail| error!("file_worker -> {:#?}", fail))
                    .unwrap();
            }
            (peer_id, FileRequest::Seek(SeekFileRequest { fd, seek_from })) => {
                let seek_result = file_manager.seek(fd, seek_from.into());
                let response = FileResponse::Seek(seek_result);

                file_response_tx
                    .send((peer_id, response))
                    .await
                    .inspect_err(|fail| error!("file_worker -> {:#?}", fail))
                    .unwrap();
            }
            (peer_id, FileRequest::Write(WriteFileRequest { fd, write_bytes })) => {
                let write_result = file_manager.write(fd, write_bytes);
                let response = FileResponse::Write(write_result);

                file_response_tx
                    .send((peer_id, response))
                    .await
                    .inspect_err(|fail| error!("file_worker -> {:#?}", fail))
                    .unwrap();
            }
            (peer_id, FileRequest::Close(CloseFileRequest { fd })) => {
                let close_result = file_manager.close(fd);
                let response = FileResponse::Close(close_result);

                file_response_tx
                    .send((peer_id, response))
                    .await
                    .inspect_err(|fail| error!("file_worker -> {:#?}", fail))
                    .unwrap();
            }
        }
    }
    debug!("file worker ends");
    Ok(())
}
