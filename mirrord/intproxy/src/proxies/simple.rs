//! The most basic proxying logic. Handles cases when the only job to do in the internal proxy is to
//! pass requests and responses between the layer and the agent.

use mirrord_protocol::{
    dns::{GetAddrInfoRequest, GetAddrInfoResponse},
    ClientMessage, FileRequest, FileResponse,
};

use crate::{
    background_tasks::{BackgroundTask, MessageBus},
    protocol::{LocalMessage, MessageId, ProxyToLayerMessage},
    request_queue::{RequestQueue, RequestQueueEmpty},
    ProxyMessage,
};

pub enum SimpleProxyMessage {
    FileReq(MessageId, FileRequest),
    FileRes(FileResponse),
    AddrInfoReq(MessageId, GetAddrInfoRequest),
    AddrInfoRes(GetAddrInfoResponse),
}

/// For passing messages between the layer and the agent without custom internal logic.
/// Run as a [`BackgroundTask`] by each [`ProxySession`](crate::session::ProxySession).
#[derive(Default)]
pub struct SimpleProxy {
    file_reqs: RequestQueue,
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
                    None => break Ok(()),
                    Some(SimpleProxyMessage::FileReq(id, req)) => {
                        self.file_reqs.insert(id);
                        message_bus.send(ProxyMessage::ToAgent(ClientMessage::FileRequest(req))).await;
                    }
                    Some(SimpleProxyMessage::FileRes(res)) => {
                        let message_id = self.file_reqs.get()?;
                        message_bus.send(ProxyMessage::ToLayer(LocalMessage { message_id, inner: ProxyToLayerMessage::File(res) })).await;
                    }
                    Some(SimpleProxyMessage::AddrInfoReq(id, req)) => {
                        self.addr_info_reqs.insert(id);
                        message_bus.send(ProxyMessage::ToAgent(ClientMessage::GetAddrInfoRequest(req))).await;
                    }
                    Some(SimpleProxyMessage::AddrInfoRes(res)) => {
                        let message_id = self.addr_info_reqs.get()?;
                        message_bus.send(ProxyMessage::ToLayer(LocalMessage { message_id, inner: ProxyToLayerMessage::GetAddrInfo(res)})).await;
                    }
                }
            }
        }
    }
}
