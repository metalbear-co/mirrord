//! The most basic proxying logic. Handles cases when the only job to do in the internal proxy is to
//! pass requests and responses between the layer and the agent.

use std::{collections::HashMap, vec::IntoIter};

use mirrord_intproxy_protocol::{LayerId, MessageId, ProxyToLayerMessage};
use mirrord_protocol::{
    dns::{GetAddrInfoRequest, GetAddrInfoResponse},
    file::{
        CloseDirRequest, CloseFileRequest, DirEntryInternal, OpenDirResponse, OpenFileResponse,
        ReadDirBatchRequest, ReadDirBatchResponse, ReadDirRequest, ReadDirResponse,
        READDIR_BATCH_VERSION,
    },
    ClientMessage, FileRequest, FileResponse, GetEnvVarsRequest, RemoteResult, ResponseError,
};
use semver::Version;
use thiserror::Error;

use crate::{
    background_tasks::{BackgroundTask, MessageBus},
    main_tasks::{LayerClosed, LayerForked, ToLayer},
    remote_resources::RemoteResources,
    request_queue::{RequestQueue, RequestQueueEmpty},
    ProxyMessage,
};

#[derive(Debug)]
pub enum SimpleProxyMessage {
    FileReq(MessageId, LayerId, FileRequest),
    FileRes(FileResponse),
    AddrInfoReq(MessageId, LayerId, GetAddrInfoRequest),
    AddrInfoRes(GetAddrInfoResponse),
    LayerForked(LayerForked),
    LayerClosed(LayerClosed),
    GetEnvReq(MessageId, LayerId, GetEnvVarsRequest),
    GetEnvRes(RemoteResult<HashMap<String, String>>),
    ProtocolVersion(Version),
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum RemoteFd {
    File(u64),
    Dir(u64),
}

#[derive(Clone)]
pub(crate) enum FileResource {
    File,
    Dir {
        dirs_iter: IntoIter<DirEntryInternal>,
    },
}

#[derive(Error, Debug)]
enum FileError {
    #[error("Resource `{0}` not found!")]
    MissingResource(u64),

    #[error("Dir operation called on file `{0}`!")]
    DirOnFile(u64),
}

impl From<FileError> for ResponseError {
    fn from(file_fail: FileError) -> Self {
        match file_fail {
            FileError::MissingResource(remote_fd) => ResponseError::NotFound(remote_fd),
            FileError::DirOnFile(remote_fd) => ResponseError::NotDirectory(remote_fd),
        }
    }
}

impl FileResource {
    fn next_dir(&mut self, remote_fd: u64) -> Result<Option<DirEntryInternal>, FileError> {
        match self {
            FileResource::Dir { dirs_iter } => dirs_iter.next().map(Ok).transpose(),
            FileResource::File => Err(FileError::DirOnFile(remote_fd)),
        }
    }
}

/// For passing messages between the layer and the agent without custom internal logic.
/// Run as a [`BackgroundTask`].
#[derive(Default)]
pub struct SimpleProxy {
    /// Remote descriptors for open files and directories. Allows tracking across layer forks.
    remote_fds: RemoteResources<RemoteFd, FileResource>,
    /// For [`FileRequest`]s.
    file_reqs: RequestQueue,
    /// For [`GetAddrInfoRequest`]s.
    addr_info_reqs: RequestQueue,
    /// For [`GetEnvVarsRequest`]s.
    get_env_reqs: RequestQueue,
}

impl SimpleProxy {
    /// `readdir` works by keeping an iterator of all the `dir`s, and a call to it is
    /// equivalent to doing `iterator.next()`.
    ///
    /// For efficiency, whenever we receive a `readdir` request from the layer, we try to
    /// read more than just `next()` from the agent, while returning just the next `direntry`
    /// back to layer.
    async fn handle_readdir(
        &mut self,
        layer_id: LayerId,
        remote_fd: u64,
        message_id: u64,
        protocol_version: Option<&Version>,
        message_bus: &mut MessageBus<SimpleProxy>,
    ) -> Result<(), FileError> {
        let resource = self
            .remote_fds
            .get_mut(&layer_id, &RemoteFd::Dir(remote_fd))
            .ok_or(FileError::MissingResource(remote_fd))?;

        if let Some(dir) = resource.next_dir(remote_fd)? {
            message_bus
                .send(ToLayer {
                    message_id,
                    message: ProxyToLayerMessage::File(FileResponse::ReadDir(Ok(
                        ReadDirResponse {
                            direntry: Some(dir),
                        },
                    ))),
                    layer_id,
                })
                .await;
        } else {
            self.file_reqs.insert(message_id, layer_id);

            let request =
                if protocol_version.is_some_and(|version| READDIR_BATCH_VERSION.matches(version)) {
                    FileRequest::ReadDirBatch(ReadDirBatchRequest {
                        remote_fd,
                        amount: 128,
                    })
                } else {
                    FileRequest::ReadDir(ReadDirRequest { remote_fd })
                };

            // Convert it into a `ReadDirBatch` for the agent.
            message_bus
                .send(ProxyMessage::ToAgent(ClientMessage::FileRequest(request)))
                .await;
        }

        Ok(())
    }
}

impl BackgroundTask for SimpleProxy {
    type Error = RequestQueueEmpty;
    type MessageIn = SimpleProxyMessage;
    type MessageOut = ProxyMessage;

    async fn run(mut self, message_bus: &mut MessageBus<Self>) -> Result<(), RequestQueueEmpty> {
        let mut protocol_version = None;

        while let Some(msg) = message_bus.recv().await {
            tracing::trace!(?msg, "new message in message_bus");

            match msg {
                SimpleProxyMessage::ProtocolVersion(new_protocol_version) => {
                    protocol_version = Some(new_protocol_version);
                }
                SimpleProxyMessage::FileReq(
                    _,
                    layer_id,
                    FileRequest::Close(CloseFileRequest { fd }),
                ) => {
                    let do_close = self.remote_fds.remove(layer_id, RemoteFd::File(fd));
                    if do_close {
                        message_bus
                            .send(ClientMessage::FileRequest(FileRequest::Close(
                                CloseFileRequest { fd },
                            )))
                            .await;
                    }
                }
                SimpleProxyMessage::FileReq(
                    _,
                    layer_id,
                    FileRequest::CloseDir(CloseDirRequest { remote_fd }),
                ) => {
                    let do_close = self.remote_fds.remove(layer_id, RemoteFd::Dir(remote_fd));
                    if do_close {
                        message_bus
                            .send(ClientMessage::FileRequest(FileRequest::CloseDir(
                                CloseDirRequest { remote_fd },
                            )))
                            .await;
                    }
                }
                SimpleProxyMessage::FileReq(
                    message_id,
                    layer_id,
                    FileRequest::ReadDir(ReadDirRequest { remote_fd }),
                ) => {
                    if let Err(fail) = self
                        .handle_readdir(
                            layer_id,
                            remote_fd,
                            message_id,
                            protocol_version.as_ref(),
                            message_bus,
                        )
                        .await
                    {
                        // Send local failure to layer.
                        message_bus
                            .send(ToLayer {
                                message_id,
                                message: ProxyToLayerMessage::File(FileResponse::ReadDir(Err(
                                    fail.into(),
                                ))),
                                layer_id,
                            })
                            .await;
                    }
                }
                SimpleProxyMessage::FileReq(message_id, session_id, req) => {
                    self.file_reqs.insert(message_id, session_id);
                    message_bus
                        .send(ProxyMessage::ToAgent(ClientMessage::FileRequest(req)))
                        .await;
                }
                SimpleProxyMessage::FileRes(FileResponse::Open(Ok(OpenFileResponse { fd }))) => {
                    let (message_id, layer_id) = self.file_reqs.get()?;

                    self.remote_fds
                        .add(layer_id, RemoteFd::File(fd), FileResource::File);

                    message_bus
                        .send(ToLayer {
                            message_id,
                            message: ProxyToLayerMessage::File(FileResponse::Open(Ok(
                                OpenFileResponse { fd },
                            ))),
                            layer_id,
                        })
                        .await;
                }
                SimpleProxyMessage::FileRes(FileResponse::OpenDir(Ok(OpenDirResponse { fd }))) => {
                    let (message_id, layer_id) = self.file_reqs.get()?;

                    self.remote_fds.add(
                        layer_id,
                        RemoteFd::Dir(fd),
                        FileResource::Dir {
                            dirs_iter: IntoIter::default(),
                        },
                    );

                    message_bus
                        .send(ToLayer {
                            message_id,
                            message: ProxyToLayerMessage::File(FileResponse::OpenDir(Ok(
                                OpenDirResponse { fd },
                            ))),
                            layer_id,
                        })
                        .await;
                }
                SimpleProxyMessage::FileRes(FileResponse::ReadDirBatch(Ok(
                    ReadDirBatchResponse { fd, dir_entries },
                ))) => {
                    let (message_id, layer_id) = self.file_reqs.get()?;

                    let mut entries_iter = dir_entries.into_iter();
                    let direntry = entries_iter.next();

                    message_bus
                        .send(ToLayer {
                            message_id,
                            message: ProxyToLayerMessage::File(FileResponse::ReadDir(Ok(
                                ReadDirResponse { direntry },
                            ))),
                            layer_id,
                        })
                        .await;

                    if let Some(FileResource::Dir { dirs_iter }) =
                        self.remote_fds.get_mut(&layer_id, &RemoteFd::Dir(fd))
                    {
                        *dirs_iter = entries_iter;
                    }
                }
                SimpleProxyMessage::FileRes(res) => {
                    let (message_id, layer_id) = self.file_reqs.get()?;
                    message_bus
                        .send(ToLayer {
                            message_id,
                            message: ProxyToLayerMessage::File(res),
                            layer_id,
                        })
                        .await;
                }
                SimpleProxyMessage::AddrInfoReq(message_id, session_id, req) => {
                    self.addr_info_reqs.insert(message_id, session_id);
                    message_bus
                        .send(ProxyMessage::ToAgent(ClientMessage::GetAddrInfoRequest(
                            req,
                        )))
                        .await;
                }
                SimpleProxyMessage::AddrInfoRes(res) => {
                    let (message_id, layer_id) = self.addr_info_reqs.get()?;
                    message_bus
                        .send(ToLayer {
                            message_id,
                            message: ProxyToLayerMessage::GetAddrInfo(res),
                            layer_id,
                        })
                        .await;
                }
                SimpleProxyMessage::LayerClosed(LayerClosed { id }) => {
                    for to_close in self.remote_fds.remove_all(id) {
                        let req = match to_close {
                            RemoteFd::Dir(remote_fd) => {
                                FileRequest::CloseDir(CloseDirRequest { remote_fd })
                            }
                            RemoteFd::File(fd) => FileRequest::Close(CloseFileRequest { fd }),
                        };

                        message_bus.send(ClientMessage::FileRequest(req)).await;
                    }
                }
                SimpleProxyMessage::LayerForked(LayerForked { child, parent }) => {
                    self.remote_fds.clone_all(parent, child);
                }
                SimpleProxyMessage::GetEnvReq(message_id, layer_id, req) => {
                    self.get_env_reqs.insert(message_id, layer_id);
                    message_bus
                        .send(ProxyMessage::ToAgent(ClientMessage::GetEnvVarsRequest(req)))
                        .await;
                }
                SimpleProxyMessage::GetEnvRes(res) => {
                    let (message_id, layer_id) = self.get_env_reqs.get()?;
                    message_bus
                        .send(ToLayer {
                            message_id,
                            message: ProxyToLayerMessage::GetEnv(res),
                            layer_id,
                        })
                        .await
                }
            }
        }

        tracing::trace!("message bus closed, exiting");
        Ok(())
    }
}

#[cfg(test)]
mod tests {

    use super::SimpleProxy;
    use crate::{
        background_tasks::{BackgroundTask, BackgroundTasks, MessageBus},
        error::IntProxyError,
        main_tasks::{MainTaskId, ProxyMessage},
    };

    #[tokio::test]
    async fn checks_protocol_version_for_readdir() {
        let mut background_tasks: BackgroundTasks<MainTaskId, ProxyMessage, IntProxyError> =
            Default::default();

        let simple_proxy =
            background_tasks.register(SimpleProxy::default(), MainTaskId::SimpleProxy, 32);

        // TODO(alex) [high]: Call send with protocol version switch, then with
        // readdirbatch message?
        // Have another test that does it without the protocol version.
    }
}
