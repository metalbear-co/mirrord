//! The most basic proxying logic. Handles cases when the only job to do in the internal proxy is to
//! pass requests and responses between the layer and the agent.

use mirrord_protocol::{
    dns::{GetAddrInfoRequest, GetAddrInfoResponse},
    ClientMessage, FileRequest, FileResponse,
};

use crate::{
    background_tasks::{BackgroundTask, MessageBus},
    main_tasks::ToLayer,
    protocol::{LayerId, MessageId, ProxyToLayerMessage},
    request_queue::{RequestQueue, RequestQueueEmpty},
    ProxyMessage,
};

pub enum SimpleProxyMessage {
    FileReq(MessageId, LayerId, FileRequest),
    FileRes(FileResponse),
    AddrInfoReq(MessageId, LayerId, GetAddrInfoRequest),
    AddrInfoRes(GetAddrInfoResponse),
}

/// For passing messages between the layer and the agent without custom internal logic.
/// Run as a [`BackgroundTask`].
#[derive(Default)]
pub struct SimpleProxy {
    /// For [`FileRequest`]s.
    file_reqs: RequestQueue,
    /// For [`GetAddrInfoRequest`]s.
    addr_info_reqs: RequestQueue,
}

impl BackgroundTask for SimpleProxy {
    type Error = RequestQueueEmpty;
    type MessageIn = SimpleProxyMessage;
    type MessageOut = ProxyMessage;

    async fn run(mut self, message_bus: &mut MessageBus<Self>) -> Result<(), RequestQueueEmpty> {
        while let Some(msg) = message_bus.recv().await {
            match msg {
                SimpleProxyMessage::FileReq(message_id, session_id, req) => {
                    self.file_reqs.insert(message_id, session_id);
                    message_bus
                        .send(ProxyMessage::ToAgent(ClientMessage::FileRequest(req)))
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
            }
        }

        tracing::trace!("message bus closed, exiting");
        Ok(())
    }
}
