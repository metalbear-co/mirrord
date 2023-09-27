use mirrord_protocol::{
    dns::{GetAddrInfoRequest, GetAddrInfoResponse},
    ClientMessage, FileRequest, FileResponse,
};

use crate::{
    agent_conn::AgentSender,
    error::{IntProxyError, Result},
    layer_conn::LayerSender,
    protocol::{LocalMessage, MessageId, ProxyToLayerMessage},
    request_queue::RequestQueue,
};

/// For passing messages between the layer and the agent without custom internal logic.
pub struct SimpleProxy {
    agent_sender: AgentSender,
    layer_sender: LayerSender,
    queue: RequestQueue,
}

impl SimpleProxy {
    pub fn new(layer_sender: LayerSender, agent_sender: AgentSender) -> Self {
        Self {
            agent_sender,
            layer_sender,
            queue: Default::default(),
        }
    }

    pub async fn handle_request<R: SimpleRequest>(
        &mut self,
        request: R,
        id: MessageId,
    ) -> Result<()> {
        if request.expects_response() {
            self.queue.save_request_id(id);
        }

        self.agent_sender
            .send(request.into_message())
            .await
            .map_err(Into::into)
    }

    pub async fn handle_response<R: SimpleResponse>(&mut self, response: R) -> Result<()> {
        let message_id = self
            .queue
            .get_request_id()
            .ok_or(IntProxyError::RequestQueueEmpty)?;

        self.layer_sender
            .send(LocalMessage {
                message_id,
                inner: response.into_message(),
            })
            .await
            .map_err(Into::into)
    }
}

pub trait SimpleRequest {
    fn into_message(self) -> ClientMessage;

    fn expects_response(&self) -> bool;
}

impl SimpleRequest for FileRequest {
    fn into_message(self) -> ClientMessage {
        ClientMessage::FileRequest(self)
    }

    fn expects_response(&self) -> bool {
        !matches!(self, Self::Close(..) | Self::CloseDir(..))
    }
}

impl SimpleRequest for GetAddrInfoRequest {
    fn into_message(self) -> ClientMessage {
        ClientMessage::GetAddrInfoRequest(self)
    }

    fn expects_response(&self) -> bool {
        true
    }
}

pub trait SimpleResponse {
    fn into_message(self) -> ProxyToLayerMessage;
}

impl SimpleResponse for FileResponse {
    fn into_message(self) -> ProxyToLayerMessage {
        ProxyToLayerMessage::File(self)
    }
}

impl SimpleResponse for GetAddrInfoResponse {
    fn into_message(self) -> ProxyToLayerMessage {
        ProxyToLayerMessage::GetAddrInfo(self)
    }
}
