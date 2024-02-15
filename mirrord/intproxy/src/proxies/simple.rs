//! The most basic proxying logic. Handles cases when the only job to do in the internal proxy is to
//! pass requests and responses between the layer and the agent.

use std::collections::HashMap;

use mirrord_intproxy_protocol::{LayerId, MessageId, ProxyToLayerMessage};
use mirrord_protocol::{
    dns::{GetAddrInfoRequest, GetAddrInfoResponse},
    file::{CloseDirRequest, CloseFileRequest, OpenDirResponse, OpenFileResponse},
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
enum RemoteFd {
    File(u64),
    Dir(u64),
}

/// For passing messages between the layer and the agent without custom internal logic.
/// Run as a [`BackgroundTask`].
#[derive(Default)]
pub struct SimpleProxy {
    /// Remote descriptors for open files and directories. Allows tracking across layer forks.
    remote_fds: RemoteResources<RemoteFd>,
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
                SimpleProxyMessage::FileReq(message_id, session_id, req) => {
                    self.file_reqs.insert(message_id, session_id);
                    message_bus
                        .send(ProxyMessage::ToAgent(ClientMessage::FileRequest(req)))
                        .await;
                }
                SimpleProxyMessage::FileRes(FileResponse::Open(Ok(OpenFileResponse { fd }))) => {
                    let (message_id, layer_id) = self.file_reqs.get()?;

                    self.remote_fds.add(layer_id, RemoteFd::File(fd));

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

                    self.remote_fds.add(layer_id, RemoteFd::Dir(fd));

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
