use derive_more::From;
use mirrord_protocol::{
    dns::{GetAddrInfoRequest, GetAddrInfoResponse},
    ClientMessage, FileRequest, FileResponse, GetEnvVarsRequest,
};
use tokio::sync::mpsc::Receiver;

use crate::{
    agent_conn::AgentSender,
    layer_conn::LayerConnector,
    protocol::{LocalMessage, MessageId, ProxyToLayerMessage},
    request_queue::{RequestQueue, RequestQueueEmpty},
    system::{Component, ComponentRef},
};

struct Queues {
    file_ops: RequestQueue<MessageId>,
    get_addr_info: RequestQueue<MessageId>,
    get_env_vars: RequestQueue<MessageId>,
}

impl Default for Queues {
    fn default() -> Self {
        Self {
            file_ops: RequestQueue::new("file_ops"),
            get_addr_info: RequestQueue::new("get_addr_info"),
            get_env_vars: RequestQueue::new("get_env_vars"),
        }
    }
}

/// For passing messages between the layer and the agent without custom internal logic.
pub struct SimpleProxy {
    agent_sender: AgentSender,
    layer_connector_ref: ComponentRef<LayerConnector>,
    queues: Queues,
}

impl SimpleProxy {
    pub fn new(
        layer_connector_ref: ComponentRef<LayerConnector>,
        agent_sender: AgentSender,
    ) -> Self {
        Self {
            agent_sender,
            layer_connector_ref,
            queues: Default::default(),
        }
    }

    async fn handle_request<R: SimpleRequest>(&mut self, request: LocalMessage<R>) {
        if let Some(queue) = request.inner.get_queue(&mut self.queues) {
            queue.insert(request.message_id);
        }

        self.agent_sender.send(request.inner.into_message()).await;
    }

    async fn handle_response<R: SimpleResponse>(
        &mut self,
        response: R,
    ) -> Result<(), RequestQueueEmpty> {
        let message_id = response.get_queue(&mut self.queues).get()?;

        self.layer_connector_ref
            .send(LocalMessage {
                message_id,
                inner: response.into_message(),
            })
            .await;

        Ok(())
    }
}

/// Messages handled by [`SimpleProxy`] component.
#[derive(From)]
pub enum SimpleProxyMessage {
    FileRequest(LocalMessage<FileRequest>),
    FileResponse(FileResponse),
    GetAddrInfoRequest(LocalMessage<GetAddrInfoRequest>),
    GetAddrInfoResponse(GetAddrInfoResponse),
}

impl Component for SimpleProxy {
    type Message = SimpleProxyMessage;
    type Id = &'static str;
    type Error = RequestQueueEmpty;

    fn id(&self) -> Self::Id {
        "SIMPLE_PROXY"
    }

    async fn run(
        mut self,
        mut message_rx: Receiver<SimpleProxyMessage>,
    ) -> Result<(), Self::Error> {
        while let Some(message) = message_rx.recv().await {
            match message {
                SimpleProxyMessage::FileRequest(req) => self.handle_request(req).await,
                SimpleProxyMessage::GetAddrInfoRequest(req) => self.handle_request(req).await,

                SimpleProxyMessage::FileResponse(res) => self.handle_response(res).await?,
                SimpleProxyMessage::GetAddrInfoResponse(res) => self.handle_response(res).await?,
            }
        }

        Ok(())
    }
}

trait SimpleRequest {
    fn into_message(self) -> ClientMessage;

    fn get_queue<'a>(&self, queues: &'a mut Queues) -> Option<&'a mut RequestQueue<MessageId>>;
}

impl SimpleRequest for FileRequest {
    fn into_message(self) -> ClientMessage {
        ClientMessage::FileRequest(self)
    }

    fn get_queue<'a>(&self, queues: &'a mut Queues) -> Option<&'a mut RequestQueue<MessageId>> {
        match self {
            Self::Close(..) | Self::CloseDir(..) => None,
            _ => Some(&mut queues.file_ops),
        }
    }
}

impl SimpleRequest for GetAddrInfoRequest {
    fn into_message(self) -> ClientMessage {
        ClientMessage::GetAddrInfoRequest(self)
    }

    fn get_queue<'a>(&self, queues: &'a mut Queues) -> Option<&'a mut RequestQueue<MessageId>> {
        Some(&mut queues.get_addr_info)
    }
}

impl SimpleRequest for GetEnvVarsRequest {
    fn into_message(self) -> ClientMessage {
        ClientMessage::GetEnvVarsRequest(self)
    }

    fn get_queue<'a>(&self, queues: &'a mut Queues) -> Option<&'a mut RequestQueue<MessageId>> {
        Some(&mut queues.get_env_vars)
    }
}

trait SimpleResponse {
    fn into_message(self) -> ProxyToLayerMessage;

    fn get_queue<'a>(&self, queues: &'a mut Queues) -> &'a mut RequestQueue<MessageId>;
}

impl SimpleResponse for FileResponse {
    fn into_message(self) -> ProxyToLayerMessage {
        ProxyToLayerMessage::File(self)
    }

    fn get_queue<'a>(&self, queues: &'a mut Queues) -> &'a mut RequestQueue<MessageId> {
        &mut queues.file_ops
    }
}

impl SimpleResponse for GetAddrInfoResponse {
    fn into_message(self) -> ProxyToLayerMessage {
        ProxyToLayerMessage::GetAddrInfo(self)
    }

    fn get_queue<'a>(&self, queues: &'a mut Queues) -> &'a mut RequestQueue<MessageId> {
        &mut queues.get_addr_info
    }
}
