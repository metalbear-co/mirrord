use core::fmt;
use std::{collections::HashMap, vec};

use mirrord_intproxy_protocol::{LayerId, MessageId, ProxyToLayerMessage};
use mirrord_protocol::{
    file::{
        CloseDirRequest, CloseFileRequest, DirEntryInternal, ReadDirBatchRequest, ReadDirResponse,
        ReadFileResponse, ReadLimitedFileRequest, READDIR_BATCH_VERSION,
    },
    ClientMessage, DaemonMessage, FileRequest, FileResponse, ResponseError,
};
use semver::Version;
use thiserror::Error;
use tracing::Level;

use crate::{
    background_tasks::{BackgroundTask, MessageBus},
    error::UnexpectedAgentMessage,
    main_tasks::{LayerClosed, LayerForked, ProxyMessage, ToLayer},
    remote_resources::RemoteResources,
    request_queue::RequestQueue,
};

/// Messages handled by [`FilesProxy`].
#[derive(Debug)]
pub enum FilesProxyMessage {
    /// Layer sent file request.
    FileReq(MessageId, LayerId, FileRequest),
    /// Agent sent file response.
    FileRes(FileResponse),
    /// Protocol version was negotiated with the agent.
    ProtocolVersion(Version),
    /// Layer instance forked.
    LayerForked(LayerForked),
    /// Layer instance closed.
    LayerClosed(LayerClosed),
}

/// Error that can occur in [`FilesProxy`].
#[derive(Error, Debug)]
#[error(transparent)]
pub struct FilesProxyError(#[from] UnexpectedAgentMessage);

/// Locally cached data of a remote file.
#[derive(Default)]
struct FileData {
    /// Buffered file contents.
    buffer: Vec<u8>,
    /// Position of [`Self::buffer`] in file.
    buffer_position: u64,
    /// Position of the file descriptor.
    fd_position: u64,
}

impl FileData {
    /// Attempts to read `amount` bytes from [`Self::buffer`], starting from `position` in the file.
    ///
    /// Returns [`None`] when the read does not fit in the buffer in whole.
    fn read_from_buffer(&self, amount: u64, position: u64) -> Option<&[u8]> {
        let start_from = position.checked_sub(self.buffer_position)? as usize;
        let end_before = start_from + amount as usize;
        self.buffer.get(start_from..end_before)
    }
}

impl fmt::Debug for FileData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileData")
            .field("buffer_position", &self.buffer_position)
            .field("buffer_len", &self.buffer.len())
            .field("fd_position", &self.fd_position)
            .finish()
    }
}

/// Locally cached data of a remote directory.
#[derive(Default)]
struct DirData {
    /// Buffered entries of this directory.
    buffered_entries: vec::IntoIter<DirEntryInternal>,
}

impl fmt::Debug for DirData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DirData")
            .field("remaining_buffered_entries", &self.buffered_entries.len())
            .finish()
    }
}

/// Additional request data that is saved by [`FilesProxy`] in its [`RequestQueue`].
/// Allows for handling buffered reads.
#[derive(Debug, Default)]
enum AdditionalRequestData {
    OpenBuffered,

    ReadBuffered {
        /// File descriptor.
        fd: u64,
        /// Read buffer size of the user application.
        requested_amount: u64,
        /// Whether fd position in file
        update_fd_position: bool,
    },

    SeekBuffered {
        /// File descriptor.
        fd: u64,
    },

    /// All other file ops.
    #[default]
    Other,
}

/// For handling all file operations.
/// Run as a [`BackgroundTask`].
pub struct FilesProxy {
    protocol_version: Option<Version>,
    buffer_reads: bool,

    request_queue: RequestQueue<AdditionalRequestData>,

    remote_file_fds: RemoteResources<u64>,
    files_data: HashMap<u64, FileData>,

    remote_dir_fds: RemoteResources<u64>,
    dirs_data: HashMap<u64, DirData>,
}

impl fmt::Debug for FilesProxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilesProxy")
            .field("buffer_read", &self.buffer_reads)
            .field("buffer_readdir", &self.buffer_dirs())
            .field("files_data", &self.files_data)
            .field("dirs_data", &self.dirs_data)
            .field("protocol_version", &self.protocol_version)
            .field("request_queue", &self.request_queue)
            .finish()
    }
}

impl FilesProxy {
    const READDIR_BATCH_SIZE: usize = 128;
    const READFILE_CHUNK_SIZE: u64 = 4096;

    pub fn new(buffer_reads: bool) -> Self {
        Self {
            protocol_version: Default::default(),
            buffer_reads,

            request_queue: Default::default(),

            remote_file_fds: Default::default(),
            files_data: Default::default(),

            remote_dir_fds: Default::default(),
            dirs_data: Default::default(),
        }
    }

    fn buffer_dirs(&self) -> bool {
        self.protocol_version
            .as_ref()
            .is_some_and(|version| READDIR_BATCH_VERSION.matches(version))
    }

    #[tracing::instrument(level = Level::DEBUG)]
    fn layer_forked(&mut self, forked: LayerForked) {
        self.remote_file_fds.clone_all(forked.parent, forked.child);
        self.remote_dir_fds.clone_all(forked.parent, forked.child);
    }

    #[tracing::instrument(level = Level::DEBUG, skip(message_bus))]
    async fn layer_closed(&mut self, closed: LayerClosed, message_bus: &mut MessageBus<Self>) {
        for fd in self.remote_file_fds.remove_all(closed.id) {
            self.files_data.remove(&fd);
            message_bus
                .send(ProxyMessage::ToAgent(ClientMessage::FileRequest(
                    FileRequest::Close(CloseFileRequest { fd }),
                )))
                .await;
        }

        for remote_fd in self.remote_dir_fds.remove_all(closed.id) {
            self.dirs_data.remove(&remote_fd);
            message_bus
                .send(ProxyMessage::ToAgent(ClientMessage::FileRequest(
                    FileRequest::CloseDir(CloseDirRequest { remote_fd }),
                )))
                .await;
        }
    }

    #[tracing::instrument(level = Level::DEBUG)]
    fn protocol_version(&mut self, version: Version) {
        self.protocol_version.replace(version);
    }

    #[tracing::instrument(level = Level::TRACE, skip(message_bus))]
    async fn file_request(
        &mut self,
        request: FileRequest,
        layer_id: LayerId,
        message_id: MessageId,
        message_bus: &mut MessageBus<Self>,
    ) {
        match request {
            // Should trigger remote close only when the fd is closed in all layer instances.
            FileRequest::Close(close) => {
                if self.remote_file_fds.remove(layer_id, close.fd) {
                    self.files_data.remove(&close.fd);
                    message_bus
                        .send(ClientMessage::FileRequest(FileRequest::Close(close)))
                        .await;
                }
            }

            // Should trigger remote close only when the fd is closed in all layer instances.
            FileRequest::CloseDir(close) => {
                if self.remote_dir_fds.remove(layer_id, close.remote_fd) {
                    self.dirs_data.remove(&close.remote_fd);
                    message_bus
                        .send(ClientMessage::FileRequest(FileRequest::CloseDir(close)))
                        .await;
                }
            }

            // May require storing additional data in the request queue.
            FileRequest::Open(open) => {
                let additional_data = (self.buffer_reads && open.open_options.is_read_only())
                    .then_some(AdditionalRequestData::OpenBuffered)
                    .unwrap_or_default();
                self.request_queue
                    .insert_with_data(message_id, layer_id, additional_data);
                message_bus
                    .send(ClientMessage::FileRequest(FileRequest::Open(open)))
                    .await;
            }

            // May require storing additional data in the request queue.
            FileRequest::OpenRelative(open) => {
                let additional_data = (self.buffer_reads && open.open_options.is_read_only())
                    .then_some(AdditionalRequestData::OpenBuffered)
                    .unwrap_or_default();
                self.request_queue
                    .insert_with_data(message_id, layer_id, additional_data);
                message_bus
                    .send(ClientMessage::FileRequest(FileRequest::OpenRelative(open)))
                    .await;
            }

            // Try to use local buffer if possible.
            FileRequest::Read(read) => match self.files_data.get_mut(&read.remote_fd) {
                // File is buffered.
                Some(data) => {
                    let from_buffer = data.read_from_buffer(read.buffer_size, data.fd_position);
                    if let Some(from_buffer) = from_buffer {
                        let bytes = from_buffer.to_vec();
                        data.fd_position += read.buffer_size;
                        message_bus
                            .send(ToLayer {
                                message_id,
                                layer_id,
                                message: ProxyToLayerMessage::File(FileResponse::Read(Ok(
                                    ReadFileResponse {
                                        bytes,
                                        read_amount: read.buffer_size,
                                    },
                                ))),
                            })
                            .await;
                    } else {
                        let additional_data = AdditionalRequestData::ReadBuffered {
                            fd: read.remote_fd,
                            requested_amount: read.buffer_size,
                            update_fd_position: true,
                        };
                        self.request_queue
                            .insert_with_data(message_id, layer_id, additional_data);
                        message_bus
                            .send(ClientMessage::FileRequest(FileRequest::ReadLimited(
                                ReadLimitedFileRequest {
                                    remote_fd: read.remote_fd,
                                    buffer_size: std::cmp::max(
                                        read.buffer_size,
                                        Self::READFILE_CHUNK_SIZE,
                                    ),
                                    start_from: data.fd_position,
                                },
                            )))
                            .await;
                    }
                }

                // File is not buffered.
                None => {
                    self.request_queue.insert(message_id, layer_id);
                    message_bus
                        .send(ClientMessage::FileRequest(FileRequest::Read(read)))
                        .await;
                }
            },

            // Try to use local buffer if possible.
            FileRequest::ReadLimited(read) => match self.files_data.get_mut(&read.remote_fd) {
                // File is buffered.
                Some(data) => {
                    let from_buffer = data.read_from_buffer(read.buffer_size, read.start_from);
                    if let Some(from_buffer) = from_buffer {
                        let bytes = from_buffer.to_vec();
                        message_bus
                            .send(ToLayer {
                                message_id,
                                layer_id,
                                message: ProxyToLayerMessage::File(FileResponse::ReadLimited(Ok(
                                    ReadFileResponse {
                                        bytes,
                                        read_amount: read.buffer_size,
                                    },
                                ))),
                            })
                            .await;
                    } else {
                        let additional_data = AdditionalRequestData::ReadBuffered {
                            fd: read.remote_fd,
                            requested_amount: read.buffer_size,
                            update_fd_position: false,
                        };
                        self.request_queue
                            .insert_with_data(message_id, layer_id, additional_data);
                        message_bus
                            .send(ClientMessage::FileRequest(FileRequest::ReadLimited(
                                ReadLimitedFileRequest {
                                    remote_fd: read.remote_fd,
                                    buffer_size: std::cmp::max(
                                        read.buffer_size,
                                        Self::READFILE_CHUNK_SIZE,
                                    ),
                                    start_from: read.start_from,
                                },
                            )))
                            .await;
                    }
                }

                // File is not buffered.
                None => {
                    self.request_queue.insert(message_id, layer_id);
                    message_bus
                        .send(ClientMessage::FileRequest(FileRequest::ReadLimited(read)))
                        .await;
                }
            },

            // Try to use local buffer if possible.
            FileRequest::ReadDir(read_dir) => match self.dirs_data.get_mut(&read_dir.remote_fd) {
                // Directory is buffered.
                Some(data) => {
                    if let Some(direntry) = data.buffered_entries.next() {
                        message_bus
                            .send(ToLayer {
                                message_id,
                                layer_id,
                                message: ProxyToLayerMessage::File(FileResponse::ReadDir(Ok(
                                    ReadDirResponse {
                                        direntry: Some(direntry),
                                    },
                                ))),
                            })
                            .await;
                    } else {
                        self.request_queue.insert(message_id, layer_id);
                        message_bus
                            .send(ClientMessage::FileRequest(FileRequest::ReadDirBatch(
                                ReadDirBatchRequest {
                                    remote_fd: read_dir.remote_fd,
                                    amount: Self::READDIR_BATCH_SIZE,
                                },
                            )))
                            .await;
                    }
                }

                // Directory is not buffered.
                None => {
                    self.request_queue.insert(message_id, layer_id);
                    message_bus
                        .send(ClientMessage::FileRequest(FileRequest::ReadDir(read_dir)))
                        .await;
                }
            },

            // Not supported in old `mirrord-protocol` versions.
            req @ FileRequest::ReadLink(..) => {
                let supported = self
                    .protocol_version
                    .as_ref()
                    .is_some_and(|version| READDIR_BATCH_VERSION.matches(version));

                if supported {
                    self.request_queue.insert(message_id, layer_id);
                    message_bus
                        .send(ProxyMessage::ToAgent(ClientMessage::FileRequest(req)))
                        .await;
                } else {
                    message_bus
                        .send(ToLayer {
                            message_id,
                            message: ProxyToLayerMessage::File(FileResponse::ReadLink(Err(
                                ResponseError::NotImplemented,
                            ))),
                            layer_id,
                        })
                        .await;
                }
            }

            // Should only be sent from intproxy, not from the layer.
            FileRequest::ReadDirBatch(..) => {
                unreachable!("ReadDirBatch request is never sent from the layer");
            }

            // May require storing additional data in the request queue.
            FileRequest::Seek(seek) => {
                let additional_data = self
                    .files_data
                    .contains_key(&seek.fd)
                    .then_some(AdditionalRequestData::SeekBuffered { fd: seek.fd })
                    .unwrap_or_default();
                self.request_queue
                    .insert_with_data(message_id, layer_id, additional_data);
                message_bus
                    .send(ClientMessage::FileRequest(FileRequest::Seek(seek)))
                    .await;
            }

            // Doesn't require any special logic.
            other => {
                self.request_queue.insert(message_id, layer_id);
                message_bus.send(ClientMessage::FileRequest(other)).await;
            }
        }
    }

    #[tracing::instrument(level = Level::TRACE, skip(message_bus))]
    async fn file_response(
        &mut self,
        response: FileResponse,
        message_bus: &mut MessageBus<Self>,
    ) -> Result<(), FilesProxyError> {
        match response {
            // Update file maps.
            FileResponse::Open(Ok(open)) => {
                let (message_id, layer_id, additional_data) =
                    self.request_queue.get_with_data().ok_or_else(|| {
                        UnexpectedAgentMessage(DaemonMessage::File(FileResponse::Open(Ok(
                            open.clone()
                        ))))
                    })?;

                self.remote_file_fds.add(layer_id, open.fd);

                if matches!(additional_data, AdditionalRequestData::OpenBuffered) {
                    self.files_data.insert(open.fd, Default::default());
                }

                message_bus
                    .send(ToLayer {
                        layer_id,
                        message_id,
                        message: ProxyToLayerMessage::File(FileResponse::Open(Ok(open))),
                    })
                    .await;
            }

            // Update dir maps.
            FileResponse::OpenDir(Ok(open)) => {
                let (message_id, layer_id) = self.request_queue.get().ok_or_else(|| {
                    UnexpectedAgentMessage(DaemonMessage::File(FileResponse::OpenDir(Ok(
                        open.clone()
                    ))))
                })?;
                self.remote_dir_fds.add(layer_id, open.fd);

                message_bus
                    .send(ToLayer {
                        layer_id,
                        message_id,
                        message: ProxyToLayerMessage::File(FileResponse::OpenDir(Ok(open))),
                    })
                    .await;
            }

            // If the file is buffered, update `files_data`.
            FileResponse::ReadLimited(Ok(read)) => {
                let (message_id, layer_id, additional_data) =
                    self.request_queue.get_with_data().ok_or_else(|| {
                        UnexpectedAgentMessage(DaemonMessage::File(FileResponse::ReadLimited(Ok(
                            read.clone(),
                        ))))
                    })?;

                let AdditionalRequestData::ReadBuffered {
                    fd,
                    requested_amount,
                    update_fd_position,
                } = additional_data
                else {
                    // This file is not buffered.
                    message_bus
                        .send(ToLayer {
                            message_id,
                            layer_id,
                            message: ProxyToLayerMessage::File(FileResponse::ReadLimited(Ok(read))),
                        })
                        .await;
                    return Ok(());
                };

                let Some(data) = self.files_data.get_mut(&fd) else {
                    // File must have been closed from other thread in user application.
                    message_bus
                        .send(ToLayer {
                            message_id,
                            layer_id,
                            message: ProxyToLayerMessage::File(FileResponse::ReadLimited(Err(
                                ResponseError::NotFound(fd),
                            ))),
                        })
                        .await;
                    return Ok(());
                };

                let bytes = read
                    .bytes
                    .get(..requested_amount as usize)
                    .unwrap_or(&read.bytes)
                    .to_vec();
                let read_amount = bytes.len() as u64;
                let response = ReadFileResponse { bytes, read_amount };

                data.buffer = read.bytes;
                data.buffer_position = data.fd_position;
                let message = if update_fd_position {
                    // User originally sent `FileRequest::Read`.
                    data.fd_position += response.read_amount;
                    FileResponse::Read(Ok(response))
                } else {
                    // User originally sent `FileRequest::ReadLimited`.
                    FileResponse::ReadLimited(Ok(response))
                };

                message_bus
                    .send(ToLayer {
                        message_id,
                        layer_id,
                        message: ProxyToLayerMessage::File(message),
                    })
                    .await;
            }

            // If the file is buffered, update `files_data`.
            FileResponse::Seek(Ok(seek)) => {
                let (message_id, layer_id, additional_data) =
                    self.request_queue.get_with_data().ok_or_else(|| {
                        UnexpectedAgentMessage(DaemonMessage::File(FileResponse::Seek(Ok(
                            seek.clone()
                        ))))
                    })?;

                if let AdditionalRequestData::SeekBuffered { fd } = additional_data {
                    let Some(data) = self.files_data.get_mut(&fd) else {
                        // File must have been closed from other thread in user application.
                        message_bus
                            .send(ToLayer {
                                message_id,
                                layer_id,
                                message: ProxyToLayerMessage::File(FileResponse::Seek(Err(
                                    ResponseError::NotFound(fd),
                                ))),
                            })
                            .await;
                        return Ok(());
                    };

                    data.fd_position = seek.result_offset;
                }

                message_bus
                    .send(ToLayer {
                        message_id,
                        layer_id,
                        message: ProxyToLayerMessage::File(FileResponse::Seek(Ok(seek))),
                    })
                    .await;
            }

            // Store extra entries in `dirs_data`.
            FileResponse::ReadDirBatch(Ok(batch)) => {
                let (message_id, layer_id) = self.request_queue.get().ok_or_else(|| {
                    UnexpectedAgentMessage(DaemonMessage::File(FileResponse::ReadDirBatch(Ok(
                        batch.clone(),
                    ))))
                })?;

                let Some(data) = self.dirs_data.get_mut(&batch.fd) else {
                    // Directory must have been closed from other thread in user application.
                    message_bus
                        .send(ToLayer {
                            message_id,
                            layer_id,
                            message: ProxyToLayerMessage::File(FileResponse::ReadDir(Err(
                                ResponseError::NotFound(batch.fd),
                            ))),
                        })
                        .await;
                    return Ok(());
                };

                let mut entries = batch.dir_entries.into_iter();
                let direntry = entries.next();
                data.buffered_entries = entries;

                message_bus
                    .send(ToLayer {
                        message_id,
                        layer_id,
                        message: ProxyToLayerMessage::File(FileResponse::ReadDir(Ok(
                            ReadDirResponse { direntry },
                        ))),
                    })
                    .await;
            }

            // Doesn't require any special logic.
            other => {
                let (message_id, layer_id) = self
                    .request_queue
                    .get()
                    .ok_or_else(|| UnexpectedAgentMessage(DaemonMessage::File(other.clone())))?;
                message_bus
                    .send(ToLayer {
                        message_id,
                        layer_id,
                        message: ProxyToLayerMessage::File(other),
                    })
                    .await;
            }
        }

        Ok(())
    }
}

impl BackgroundTask for FilesProxy {
    type MessageIn = FilesProxyMessage;
    type MessageOut = ProxyMessage;
    type Error = FilesProxyError;

    async fn run(mut self, message_bus: &mut MessageBus<Self>) -> Result<(), Self::Error> {
        while let Some(message) = message_bus.recv().await {
            tracing::trace!(?message, "new message in message_bus");

            match message {
                FilesProxyMessage::FileReq(message_id, layer_id, request) => {
                    self.file_request(request, layer_id, message_id, message_bus)
                        .await;
                }
                FilesProxyMessage::FileRes(response) => {
                    self.file_response(response, message_bus).await?;
                }
                FilesProxyMessage::LayerClosed(closed) => {
                    self.layer_closed(closed, message_bus).await;
                }
                FilesProxyMessage::LayerForked(forked) => self.layer_forked(forked),
                FilesProxyMessage::ProtocolVersion(version) => self.protocol_version(version),
            }
        }

        tracing::trace!("message bus closed, exiting");

        Ok(())
    }
}

// TODO fix tests
// #[cfg(test)]
// mod tests {

//     use mirrord_intproxy_protocol::{LayerId, ProxyToLayerMessage};
//     use mirrord_protocol::{
//         file::{
//             FdOpenDirRequest, OpenDirResponse, ReadDirBatchRequest, ReadDirBatchResponse,
//             ReadDirRequest, ReadDirResponse,
//         },
//         ClientMessage, FileRequest, FileResponse,
//     };
//     use semver::Version;

//     use super::SimpleProxy;
//     use crate::{
//         background_tasks::{BackgroundTasks, TaskSender, TaskUpdate},
//         error::IntProxyError,
//         main_tasks::{MainTaskId, ProxyMessage, ToLayer},
//         proxies::simple::SimpleProxyMessage,
//     };

//     /// Sets up a [`TaskSender`] and [`BackgroundTasks`] for a functioning [`SimpleProxy`].
//     ///
//     /// - `protocol_version`: allows specifying the version of the protocol to use for testing out
//     ///   potential mismatches in messages.
//     async fn setup_proxy(
//         protocol_version: Version,
//     ) -> (
//         TaskSender<SimpleProxy>,
//         BackgroundTasks<MainTaskId, ProxyMessage, IntProxyError>,
//     ) {
//         let mut tasks: BackgroundTasks<MainTaskId, ProxyMessage, IntProxyError> =
//             Default::default();

//         let proxy = tasks.register(SimpleProxy::new(false), MainTaskId::SimpleProxy, 32);

//         proxy
//             .send(SimpleProxyMessage::ProtocolVersion(protocol_version))
//             .await;

//         (proxy, tasks)
//     }

//     /// Convenience for opening a dir.
//     async fn prepare_dir(
//         proxy: &TaskSender<SimpleProxy>,
//         tasks: &mut BackgroundTasks<MainTaskId, ProxyMessage, IntProxyError>,
//     ) {
//         let request = FileRequest::FdOpenDir(FdOpenDirRequest { remote_fd: 0xdad });
//         proxy
//             .send(SimpleProxyMessage::FileReq(0xbad, LayerId(0xa55), request))
//             .await;
//         let (_, update) = tasks.next().await.unzip();

//         assert!(
//             matches!(
//                 update,
//                 Some(TaskUpdate::Message(ProxyMessage::ToAgent(
//                     ClientMessage::FileRequest(FileRequest::FdOpenDir(FdOpenDirRequest { .. }),)
//                 )))
//             ),
//             "Mismatched message for `FdOpenDirRequest` {update:?}!"
//         );

//         let response = FileResponse::OpenDir(Ok(OpenDirResponse { fd: 0xdad }));
//         proxy.send(SimpleProxyMessage::FileRes(response)).await;
//         let (_, update) = tasks.next().await.unzip();

//         assert!(
//             matches!(
//                 update,
//                 Some(TaskUpdate::Message(ProxyMessage::ToLayer(ToLayer {
//                     message_id: 0xbad,
//                     layer_id: LayerId(0xa55),
//                     message: ProxyToLayerMessage::File(FileResponse::OpenDir(Ok(
//                         OpenDirResponse { .. }
//                     )))
//                 })))
//             ),
//             "Mismatched message for `OpenDirResponse` {update:?}!"
//         );
//     }

//     #[tokio::test]
//     async fn old_protocol_uses_read_dir_request() {
//         let (proxy, mut tasks) = setup_proxy(Version::new(0, 1, 0)).await;

//         prepare_dir(&proxy, &mut tasks).await;

//         let readdir_request = FileRequest::ReadDir(ReadDirRequest { remote_fd: 0xdad });
//         proxy
//             .send(SimpleProxyMessage::FileReq(
//                 0xbad,
//                 LayerId(0xa55),
//                 readdir_request,
//             ))
//             .await;
//         let (_, update) = tasks.next().await.unzip();

//         assert!(
//             matches!(
//                 update,
//                 Some(TaskUpdate::Message(ProxyMessage::ToAgent(
//                     ClientMessage::FileRequest(FileRequest::ReadDir(ReadDirRequest { .. }))
//                 )))
//             ),
//             "Mismatched message for `ReadDirRequest` {update:?}!"
//         );

//         let readdir_response = FileResponse::ReadDir(Ok(ReadDirResponse { direntry: None }));
//         proxy
//             .send(SimpleProxyMessage::FileRes(readdir_response))
//             .await;
//         let (_, update) = tasks.next().await.unzip();

//         assert!(
//             matches!(
//                 update,
//                 Some(TaskUpdate::Message(ProxyMessage::ToLayer(ToLayer {
//                     message_id: 0xbad,
//                     layer_id: LayerId(0xa55),
//                     message: ProxyToLayerMessage::File(FileResponse::ReadDir(Ok(
//                         ReadDirResponse { .. }
//                     )))
//                 })))
//             ),
//             "Mismatched message for `ReadDirResponse` {update:?}!"
//         );

//         drop(proxy);
//         let results = tasks.results().await;
//         for (_, result) in results {
//             assert!(result.is_ok(), "{result:?}");
//         }
//     }

//     #[tokio::test]
//     async fn new_protocol_uses_read_dir_batch_request() {
//         let (proxy, mut tasks) = setup_proxy(Version::new(1, 8, 3)).await;

//         prepare_dir(&proxy, &mut tasks).await;

//         let request = FileRequest::ReadDirBatch(ReadDirBatchRequest {
//             remote_fd: 0xdad,
//             amount: 0xca7,
//         });
//         proxy
//             .send(SimpleProxyMessage::FileReq(0xbad, LayerId(0xa55), request))
//             .await;
//         let (_, update) = tasks.next().await.unzip();

//         assert!(
//             matches!(
//                 update,
//                 Some(TaskUpdate::Message(ProxyMessage::ToAgent(
//                     ClientMessage::FileRequest(FileRequest::ReadDirBatch(ReadDirBatchRequest {
//                         remote_fd: 0xdad,
//                         amount: 0xca7
//                     }))
//                 )))
//             ),
//             "Mismatched message for `ReadDirBatchRequest` {update:?}!"
//         );

//         let response = FileResponse::ReadDirBatch(Ok(ReadDirBatchResponse {
//             fd: 0xdad,
//             dir_entries: Vec::new(),
//         }));
//         proxy.send(SimpleProxyMessage::FileRes(response)).await;
//         let (_, update) = tasks.next().await.unzip();

//         assert!(
//             matches!(
//                 update,
//                 Some(TaskUpdate::Message(ProxyMessage::ToLayer(ToLayer {
//                     message_id: 0xbad,
//                     layer_id: LayerId(0xa55),
//                     message: ProxyToLayerMessage::File(FileResponse::ReadDir(Ok(
//                         ReadDirResponse { .. }
//                     )))
//                 })))
//             ),
//             "Mismatched message for `ReadDirBatchResponse` {update:?}!"
//         );

//         drop(proxy);
//         let results = tasks.results().await;
//         for (_, result) in results {
//             assert!(result.is_ok(), "{result:?}");
//         }
//     }
// }
