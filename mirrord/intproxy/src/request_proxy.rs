use std::marker::PhantomData;

use mirrord_protocol::{
    dns::GetAddrInfoRequest,
    file::{
        AccessFileRequest, FdOpenDirRequest, GetDEnts64Request, OpenFileRequest,
        OpenRelativeFileRequest, ReadDirRequest, ReadFileRequest, ReadLimitedFileRequest,
        SeekFileRequest, WriteFileRequest, WriteLimitedFileRequest, XstatFsRequest, XstatRequest,
    },
};

use crate::{
    agent_conn::AgentSender,
    error::{IntProxyError, Result},
    layer_conn::LayerSender,
    protocol::{
        AgentResponse, IsLayerRequestWithResponse, LayerRequest, LocalMessage, MessageId,
        ProxyToLayerMessage,
    },
    request_queue::RequestQueue,
};

trait FilteringRequestQueue: Send + Sync {
    fn save_request_id(&mut self, id: MessageId, request: &LayerRequest) -> bool;

    fn get_request_id(&mut self, response: &AgentResponse) -> Option<MessageId>;
}

struct TypedRequestQueue<T> {
    inner: RequestQueue,
    _phantom: PhantomData<fn() -> T>,
}

impl<T> Default for TypedRequestQueue<T> {
    fn default() -> Self {
        Self {
            inner: Default::default(),
            _phantom: Default::default(),
        }
    }
}

impl<T: IsLayerRequestWithResponse> FilteringRequestQueue for TypedRequestQueue<T> {
    fn save_request_id(&mut self, id: MessageId, request: &LayerRequest) -> bool {
        if T::check(request) {
            self.inner.save_request_id(id);
            true
        } else {
            false
        }
    }

    fn get_request_id(&mut self, response: &AgentResponse) -> Option<MessageId> {
        if T::check_response(response) {
            self.inner.get_request_id()
        } else {
            None
        }
    }
}

pub struct RequestProxy {
    agent_sender: AgentSender,
    layer_sender: LayerSender,
    queues: [Box<dyn FilteringRequestQueue>; 14],
}

impl RequestProxy {
    fn make_queue<T: 'static + IsLayerRequestWithResponse>() -> Box<dyn FilteringRequestQueue> {
        Box::new(TypedRequestQueue::<T>::default())
    }

    pub fn new(agent_sender: AgentSender, layer_sender: LayerSender) -> Self {
        let queues: [Box<dyn FilteringRequestQueue>; 14] = [
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
                todo!()
            }
        }
        tracing::trace!("no request queue found for {request:?}");

        self.agent_sender
            .send(request.into())
            .await
            .map_err(Into::into)
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
}
