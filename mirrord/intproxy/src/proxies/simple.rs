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
};

#[derive(Default)]
pub struct Queues {
    file_ops: RequestQueue<()>,
    get_addr_info: RequestQueue<()>,
    get_env_vars: RequestQueue<()>,
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

    pub async fn handle_request<R: SimpleRequest>(
        &mut self,
        request: R,
        id: MessageId,
    ) -> Result<()> {
        if let Some(queue) = request.get_queue(&mut self.queues) {
            queue.insert((), id);
        }

        self.agent_sender
            .send(request.into_message())
            .await
            .map_err(Into::into)
    }

    pub async fn handle_response<R: SimpleResponse>(&mut self, response: R) -> Result<()> {
        let message_id = response.get_queue(&mut self.queues).get()?.id;

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

    fn get_queue<'a>(&self, queues: &'a mut Queues) -> Option<&'a mut RequestQueue<()>>;
}

impl SimpleRequest for FileRequest {
    fn into_message(self) -> ClientMessage {
        ClientMessage::FileRequest(self)
    }

    fn get_queue<'a>(&self, queues: &'a mut Queues) -> Option<&'a mut RequestQueue<()>> {
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

    fn get_queue<'a>(&self, queues: &'a mut Queues) -> Option<&'a mut RequestQueue<()>> {
        Some(&mut queues.get_addr_info)
    }
}

impl SimpleRequest for GetEnvVarsRequest {
    fn into_message(self) -> ClientMessage {
        ClientMessage::GetEnvVarsRequest(self)
    }

    fn get_queue<'a>(&self, queues: &'a mut Queues) -> Option<&'a mut RequestQueue<()>> {
        Some(&mut queues.get_env_vars)
    }
}

pub trait SimpleResponse {
    fn into_message(self) -> ProxyToLayerMessage;

    fn get_queue<'a>(&self, queues: &'a mut Queues) -> &'a mut RequestQueue<()>;
}

impl SimpleResponse for FileResponse {
    fn into_message(self) -> ProxyToLayerMessage {
        ProxyToLayerMessage::File(self)
    }

    fn get_queue<'a>(&self, queues: &'a mut Queues) -> &'a mut RequestQueue<()> {
        &mut queues.file_ops
    }
}

impl SimpleResponse for GetAddrInfoResponse {
    fn into_message(self) -> ProxyToLayerMessage {
        ProxyToLayerMessage::GetAddrInfo(self)
    }

    fn get_queue<'a>(&self, queues: &'a mut Queues) -> &'a mut RequestQueue<()> {
        &mut queues.get_addr_info
    }
}
