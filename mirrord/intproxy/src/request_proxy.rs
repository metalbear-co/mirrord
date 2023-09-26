use std::{collections::VecDeque, marker::PhantomData};

use mirrord_protocol::{
    dns::GetAddrInfoRequest,
    file::{
        AccessFileRequest, FdOpenDirRequest, GetDEnts64Request, OpenFileRequest,
        OpenRelativeFileRequest, ReadDirRequest, ReadFileRequest, ReadLimitedFileRequest,
        SeekFileRequest, WriteFileRequest, WriteLimitedFileRequest, XstatFsRequest, XstatRequest,
    },
    ClientMessage, outgoing::{udp::LayerUdpOutgoing, LayerConnect, tcp::LayerTcpOutgoing},
};

use crate::{
    agent_conn::AgentSender,
    error::{IntProxyError, Result},
    layer_conn::LayerSender,
    protocol::{
        AgentResponse, IsLayerRequestWithResponse, LocalMessage, MessageId, ProxyToLayerMessage, LayerRequest,
    },
};

trait RequestQueue: Send + Sync {
    fn save_request_id(&mut self, id: MessageId, request: &LayerRequest) -> bool;

    fn get_request_id(&mut self, response: &AgentResponse) -> Option<MessageId>;
}

struct TypedRequestQueue<T> {
    request_ids: VecDeque<MessageId>,
    _phantom: PhantomData<fn() -> T>,
}

impl<T> Default for TypedRequestQueue<T> {
    fn default() -> Self {
        Self {
            request_ids: Default::default(),
            _phantom: Default::default(),
        }
    }
}

impl<T: IsLayerRequestWithResponse> RequestQueue for TypedRequestQueue<T> {
    fn save_request_id(&mut self, id: MessageId, request: &LayerRequest) -> bool {
        if T::check(request) {
            self.request_ids.push_back(id);
            true
        } else {
            false
        }
    }

    fn get_request_id(&mut self, response: &AgentResponse) -> Option<MessageId> {
        if T::check_response(response) {
            self.request_ids.pop_front()
        } else {
            None
        }
    }
}

pub struct RequestProxy {
    agent_sender: AgentSender,
    layer_sender: LayerSender,
    queues: [Box<dyn RequestQueue>; 14],
}

impl RequestProxy {
    fn make_queue<T: 'static + IsLayerRequestWithResponse>() -> Box<dyn RequestQueue> {
        Box::new(TypedRequestQueue::<T>::default())
    }

    pub fn new(agent_sender: AgentSender, layer_sender: LayerSender) -> Self {
        let queues: [Box<dyn RequestQueue>; 14] = [
            Self::make_queue::<OpenFileRequest>(),
            Self::make_queue::<OpenRelativeFileRequest>(),
            Self::make_queue::<ReadFileRequest>(),
            Self::make_queue::<ReadLimitedFileRequest>(),
            Self::make_queue::<SeekFileRequest>(),
            Self::make_queue::<WriteFileRequest>(),
            Self::make_queue::<WriteLimitedFileRequest>(),
            Self::make_queue::<AccessFileRequest>(),
            Self::make_queue::<XstatRequest>(),
            Self::make_queue::<XstatFsRequest>(),
            Self::make_queue::<FdOpenDirRequest>(),
            Self::make_queue::<ReadDirRequest>(),
            Self::make_queue::<GetDEnts64Request>(),
            Self::make_queue::<GetAddrInfoRequest>(),
        ];

        Self {
            agent_sender,
            layer_sender,
            queues,
        }
    }

    #[tracing::instrument(level = "trace", skip(self))]
    pub async fn handle_layer_request(
        &mut self,
        request_id: MessageId,
        request: LayerRequest,
    ) -> Result<()> {
        for queue in &mut self.queues {
            if queue.save_request_id(request_id, &request) {
                let message = Self::make_client_message(request);
                return self.agent_sender.send(message).await.map_err(Into::into);
            }
        }

        tracing::trace!("no request queue found for {request:?}");

        Ok(())
    }

    #[tracing::instrument(level = "trace", skip(self))]
    pub async fn handle_agent_response(&mut self, response: AgentResponse) -> Result<()> {
        for queue in &mut self.queues {
            let Some(message_id) = queue.get_request_id(&response) else {
                continue;
            };

            return self
                .layer_sender
                .send(LocalMessage {
                    message_id,
                    inner: ProxyToLayerMessage::AgentResponse(response),
                })
                .await
                .map_err(Into::into);
        }

        Err(IntProxyError::RequestQueueNotFound(response))
    }

    fn make_client_message(request: LayerRequest) -> ClientMessage {
        match request {
            LayerRequest::File(req) => ClientMessage::FileRequest(req),
            LayerRequest::GetAddrInfo(req) => ClientMessage::GetAddrInfoRequest(req),
            LayerRequest::ConnectUdpOutgoing(req) => ClientMessage::UdpOutgoing(LayerUdpOutgoing::Connect(LayerConnect { remote_address: req.remote_address })),
            LayerRequest::ConnectTcpOutgoing(req) => ClientMessage::TcpOutgoing(LayerTcpOutgoing::Connect(LayerConnect { remote_address: req.remote_address })),
        }
    }
}
