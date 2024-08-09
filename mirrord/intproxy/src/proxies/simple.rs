//! The most basic proxying logic. Handles cases when the only job to do in the internal proxy is to
//! pass requests and responses between the layer and the agent.

use std::{collections::HashMap, vec::IntoIter};

use mirrord_intproxy_protocol::{LayerId, MessageId, ProxyToLayerMessage};
use mirrord_protocol::{
    dns::{GetAddrInfoRequest, GetAddrInfoResponse},
    file::{
        CloseDirRequest, CloseFileRequest, DirEntryInternal, OpenDirResponse, OpenFileResponse,
        ReadDirBatchRequest, ReadDirBatchResponse, ReadDirRequest, ReadDirResponse,
    },
    ClientMessage, FileRequest, FileResponse, GetEnvVarsRequest, RemoteResult,
};

use crate::{
    background_tasks::{BackgroundTask, MessageBus},
    main_tasks::{LayerClosed, LayerForked, ToLayer},
    remote_resources::RemoteResources,
    request_queue::{RequestQueue, RequestQueueEmpty},
    ProxyMessage,
};

pub enum SimpleProxyMessage {
    FileReq(MessageId, LayerId, FileRequest),
    FileRes(FileResponse),
    AddrInfoReq(MessageId, LayerId, GetAddrInfoRequest),
    AddrInfoRes(GetAddrInfoResponse),
    LayerForked(LayerForked),
    LayerClosed(LayerClosed),
    GetEnvReq(MessageId, LayerId, GetEnvVarsRequest),
    GetEnvRes(RemoteResult<HashMap<String, String>>),
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum RemoteFd {
    File(u64),
    Dir(u64),
}

#[derive(Clone)]
pub(crate) enum FileResource {
    File,
    Dir {
        cursor: Option<IntoIter<DirEntryInternal>>,
    },
}

impl FileResource {
    fn next_dir(&mut self) -> Option<DirEntryInternal> {
        match self {
            FileResource::Dir { cursor } => cursor.as_mut().and_then(|entry| entry.next()),
            _ => None,
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

impl BackgroundTask for SimpleProxy {
    type Error = RequestQueueEmpty;
    type MessageIn = SimpleProxyMessage;
    type MessageOut = ProxyMessage;

    async fn run(mut self, message_bus: &mut MessageBus<Self>) -> Result<(), RequestQueueEmpty> {
        while let Some(msg) = message_bus.recv().await {
            match msg {
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
                    if let Some(dir) = self
                        .remote_fds
                        .get_mut(&layer_id, &RemoteFd::Dir(remote_fd))
                        .and_then(FileResource::next_dir)
                    {
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
                        // Convert it into a `ReadDirBatch` for the agent.
                        message_bus
                            .send(ProxyMessage::ToAgent(ClientMessage::FileRequest(
                                FileRequest::ReadDirBatch(ReadDirBatchRequest {
                                    remote_fd,
                                    amount: 128,
                                }),
                            )))
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
                        FileResource::Dir { cursor: None },
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
                    tracing::info!(fd, ?dir_entries, "we received something from the agent");
                    let (message_id, layer_id) = self.file_reqs.get()?;

                    let mut entries_iter = dir_entries.map(|dirs| dirs.into_iter());

                    let direntry = match entries_iter.as_mut() {
                        Some(dirs) => dirs.next(),
                        None => None,
                    };

                    message_bus
                        .send(ToLayer {
                            message_id,
                            message: ProxyToLayerMessage::File(FileResponse::ReadDir(Ok(
                                ReadDirResponse { direntry },
                            ))),
                            layer_id,
                        })
                        .await;

                    if let Some(dirs) = entries_iter.as_mut() {
                        tracing::info!(?dirs, "lets pop the first dir and save the rest");
                    }

                    if let Some(FileResource::Dir { cursor }) =
                        self.remote_fds.get_mut(&layer_id, &RemoteFd::Dir(fd))
                    {
                        tracing::info!(?cursor, "setting the response");
                        *cursor = entries_iter;
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
