//! The most basic proxying logic. Handles cases when the only job to do in the internal proxy is to
//! pass requests and responses between the layer and the agent.

use mirrord_protocol::{
    dns::{GetAddrInfoRequest, GetAddrInfoResponse},
    ClientMessage, FileRequest, FileResponse,
};

use crate::{
    background_tasks::{BackgroundTask, MessageBus},
    protocol::{LocalMessage, MessageId, ProxyToLayerMessage, SessionId},
    request_queue::{RequestQueue, RequestQueueEmpty},
    session::ProxyMessage,
};

pub enum SimpleProxyMessage {
    FileReq(MessageId, SessionId, FileRequest),
    FileRes(FileResponse),
    AddrInfoReq(MessageId, SessionId, GetAddrInfoRequest),
    AddrInfoRes(GetAddrInfoResponse),
}

/// For passing messages between the layer and the agent without custom internal logic.
/// Run as a [`BackgroundTask`] by each [`ProxySession`](crate::session::ProxySession).
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
        loop {
            tokio::select! {
                msg = message_bus.recv() => match msg {
                    None => {
                        tracing::trace!("message bus closed, exiting");
                        break Ok(());
                    },
                    Some(SimpleProxyMessage::FileReq(message_id, session_id, req)) => {
                        self.file_reqs.insert(message_id, session_id);
                        message_bus.send(ProxyMessage::ToAgent(ClientMessage::FileRequest(req))).await;
                    }
                    Some(SimpleProxyMessage::FileRes(res)) => {
                        let (message_id, session_id) = self.file_reqs.get()?;
                        message_bus.send(ProxyMessage::ToLayer(LocalMessage { message_id, inner: ProxyToLayerMessage::File(res) }, session_id)).await;
                    }
                    Some(SimpleProxyMessage::AddrInfoReq(message_id, session_id, req)) => {
                        self.addr_info_reqs.insert(message_id, session_id);
                        message_bus.send(ProxyMessage::ToAgent(ClientMessage::GetAddrInfoRequest(req))).await;
                    }
                    Some(SimpleProxyMessage::AddrInfoRes(res)) => {
                        let (message_id, session_id) = self.addr_info_reqs.get()?;
                        message_bus.send(ProxyMessage::ToLayer(LocalMessage { message_id, inner: ProxyToLayerMessage::GetAddrInfo(res)}, session_id)).await;
                    }
                }
            }
        }
    }
}
