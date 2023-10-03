use derive_more::From;
use mirrord_protocol::{
    dns::{GetAddrInfoRequest, GetAddrInfoResponse},
    ClientMessage, FileRequest, FileResponse, GetEnvVarsRequest,
};

use crate::{
    agent_conn::AgentSender,
    error::Result,
    layer_conn::LayerSender,
    protocol::{LocalMessage, MessageId, ProxyToLayerMessage},
    request_queue::RequestQueue,
    system::Component,
};

#[derive(Default)]
struct Queues {
    file_ops: RequestQueue<MessageId>,
    get_addr_info: RequestQueue<MessageId>,
    get_env_vars: RequestQueue<MessageId>,
}

/// For passing messages between the layer and the agent without custom internal logic.
pub struct SimpleProxy {
    agent_sender: AgentSender,
    layer_sender: LayerSender,
    queues: Queues,
}

impl SimpleProxy {
    pub fn new(layer_sender: LayerSender, agent_sender: AgentSender) -> Self {
        Self {
            agent_sender,
            layer_sender,
            queues: Default::default(),
        }
    }

    async fn handle_request<R: SimpleRequest>(&mut self, request: LocalMessage<R>) -> Result<()> {
        if let Some(queue) = request.inner.get_queue(&mut self.queues) {
            queue.insert(request.message_id);
        }

        self.agent_sender
            .send(request.inner.into_message())
            .await
            .map_err(Into::into)
    }

    async fn handle_response<R: SimpleResponse>(&mut self, response: R) -> Result<()> {
        let message_id = response.get_queue(&mut self.queues).get()?;

        self.layer_sender
            .send(LocalMessage {
                message_id,
                inner: response.into_message(),
            })
            .await
            .map_err(Into::into)
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

    fn id(&self) -> Self::Id {
        "SIMPLE_PROXY"
    }

    async fn handle(&mut self, message: SimpleProxyMessage) -> Result<()> {
        match message {
            SimpleProxyMessage::FileRequest(req) => self.handle_request(req).await,
            SimpleProxyMessage::GetAddrInfoRequest(req) => self.handle_request(req).await,

            SimpleProxyMessage::FileResponse(res) => self.handle_response(res).await,
            SimpleProxyMessage::GetAddrInfoResponse(res) => self.handle_response(res).await,
        }
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
