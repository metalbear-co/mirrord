use std::{collections::HashMap, marker::PhantomData};

use mirrord_protocol::{
    outgoing::{DaemonConnect, DaemonRead},
    ConnectionId, RemoteResult,
};

use super::AgentOutgoingMessage;
use crate::{
    agent_conn::AgentSender,
    error::{IntProxyError, Result},
    layer_conn::LayerSender,
    protocol::{LocalMessage, MessageId, OutgoingConnectResponse},
    proxies::outgoing::{
        interceptor::{InterceptorTask, InterceptorTaskHandle},
        Listener, NetProtocolHandler,
    },
    request_queue::RequestQueue,
};

pub struct OutgoingProxy<P> {
    agent_sender: AgentSender,
    layer_sender: LayerSender,
    interceptors: HashMap<ConnectionId, InterceptorTaskHandle>,
    connect_queue: RequestQueue,
    _phantom: PhantomData<fn() -> P>,
}

impl<P: NetProtocolHandler> OutgoingProxy<P> {
    pub fn new(agent_sender: AgentSender, layer_sender: LayerSender) -> Self {
        Self {
            agent_sender,
            layer_sender,
            interceptors: Default::default(),
            connect_queue: Default::default(),
            _phantom: Default::default(),
        }
    }

    pub async fn handle_layer_connect_request(
        &mut self,
        message: P::LayerConnectRequest,
        message_id: MessageId,
    ) -> Result<()> {
        self.connect_queue.save_request_id(message_id);

        let remote_address = P::unwrap_layer_connect_request(message);

        self.agent_sender
            .send(P::make_connect_message(remote_address))
            .await
            .map_err(Into::into)
    }

    pub async fn handle_agent_message(&mut self, message: P::AgentMessage) -> Result<()> {
        match P::unwrap_agent_message(message) {
            AgentOutgoingMessage::Connect(connect) => self.handle_agent_connect(connect).await,
            AgentOutgoingMessage::Read(read) => self.handle_agent_read(read).await,
            AgentOutgoingMessage::Close(connection_id) => {
                self.handle_agent_close(connection_id);
                Ok(())
            }
        }
    }

    fn handle_agent_close(&mut self, connection_id: ConnectionId) {
        self.interceptors.remove(&connection_id);
    }

    async fn handle_agent_read(&mut self, read: RemoteResult<DaemonRead>) -> Result<()> {
        let DaemonRead {
            connection_id,
            bytes,
        } = read?;

        let sender = self
            .interceptors
            .get_mut(&connection_id)
            .ok_or(IntProxyError::NoConnectionId(connection_id))?;

        let _ = sender.send(bytes).await;

        Ok(())
    }

    async fn handle_agent_connect(&mut self, connect: RemoteResult<DaemonConnect>) -> Result<()> {
        let message_id = self
            .connect_queue
            .get_request_id()
            .ok_or(IntProxyError::RequestQueueEmpty)?;

        let connect = match connect {
            Ok(connect) => connect,
            Err(e) => {
                return self
                    .layer_sender
                    .send(LocalMessage {
                        message_id,
                        inner: P::wrap_layer_connect_response(Err(e)),
                    })
                    .await
                    .map_err(Into::into)
            }
        };

        let DaemonConnect {
            connection_id,
            remote_address,
            local_address,
        } = connect;

        let listener = P::Listener::create(remote_address).await?;
        let layer_address = listener.local_address()?;

        let task = InterceptorTask::new(self.agent_sender.clone(), connection_id, 512);
        let handle = task.handle();
        self.interceptors.insert(connection_id, handle);

        tokio::spawn(task.run::<P>(listener));

        let response = Ok(OutgoingConnectResponse {
            user_address: local_address,
            layer_address,
        });

        self.layer_sender
            .send(LocalMessage {
                message_id,
                inner: P::wrap_layer_connect_response(response),
            })
            .await
            .map_err(Into::into)
    }
}
