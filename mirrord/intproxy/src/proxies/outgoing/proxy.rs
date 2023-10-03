use std::{collections::HashMap, io};

use derive_more::From;
use mirrord_protocol::{
    outgoing::{tcp::DaemonTcpOutgoing, udp::DaemonUdpOutgoing, DaemonConnect, DaemonRead},
    ConnectionId, RemoteResult,
};
use thiserror::Error;
use tokio::sync::mpsc::Receiver;

use super::interceptor::{InterceptorId, OutgoingInterceptor};
use crate::{
    agent_conn::AgentSender,
    layer_conn::LayerConnector,
    protocol::{
        LocalMessage, MessageId, NetProtocol, OutgoingConnectRequest, OutgoingConnectResponse,
        ProxyToLayerMessage,
    },
    request_queue::{RequestQueue, RequestQueueEmpty},
    system::{Component, ComponentError, ComponentRef, ComponentResult, System},
};

struct Queues {
    datagrams: RequestQueue<MessageId>,
    stream: RequestQueue<MessageId>,
}

impl Default for Queues {
    fn default() -> Self {
        Self {
            datagrams: RequestQueue::new("connect_datagrams"),
            stream: RequestQueue::new("connect_stream"),
        }
    }
}

impl Queues {
    fn get(&mut self, protocol: NetProtocol) -> &mut RequestQueue<MessageId> {
        match protocol {
            NetProtocol::Datagrams => &mut self.datagrams,
            NetProtocol::Stream => &mut self.stream,
        }
    }
}

pub struct OutgoingProxy {
    agent_sender: AgentSender,
    layer_connector_ref: ComponentRef<LayerConnector>,
    interceptors: HashMap<InterceptorId, ComponentRef<OutgoingInterceptor>>,
    queues: Queues,
    system: System<InterceptorId, io::Error>,
}

impl OutgoingProxy {
    pub fn new(
        layer_connector_ref: ComponentRef<LayerConnector>,
        agent_sender: AgentSender,
    ) -> Self {
        Self {
            agent_sender,
            layer_connector_ref,
            interceptors: Default::default(),
            queues: Default::default(),
            system: Default::default(),
        }
    }

    fn handle_agent_close(&mut self, connection_id: ConnectionId, protocol: NetProtocol) {
        self.interceptors.remove(&InterceptorId {
            connection_id,
            protocol,
        });
    }

    async fn handle_agent_read(&self, read: RemoteResult<DaemonRead>, protocol: NetProtocol) {
        let Ok(DaemonRead {
            connection_id,
            bytes,
        }) = read
        else {
            return;
        };

        let Some(interceptor_ref) = self.interceptors.get(&InterceptorId {
            connection_id,
            protocol,
        }) else {
            return;
        };

        interceptor_ref.send(bytes).await;
    }

    async fn handle_agent_connect(
        &mut self,
        connect: RemoteResult<DaemonConnect>,
        protocol: NetProtocol,
    ) -> Result<(), OutgoingProxyError> {
        let message_id = self.queues.get(protocol).get()?;

        let connect = match connect {
            Ok(connect) => connect,
            Err(e) => {
                self.layer_connector_ref
                    .send(LocalMessage {
                        message_id,
                        inner: ProxyToLayerMessage::OutgoingConnect(Err(e)),
                    })
                    .await;

                return Ok(());
            }
        };

        let DaemonConnect {
            connection_id,
            remote_address,
            local_address,
        } = connect;

        let prepared_socket = protocol.prepare_socket(remote_address).await?;
        let layer_address = prepared_socket.local_address()?;

        let interceptor = OutgoingInterceptor::new(
            self.agent_sender.clone(),
            connection_id,
            protocol,
            prepared_socket,
        );
        let id = interceptor.id();
        let interceptor_ref = self.system.register(interceptor);
        self.interceptors.insert(id, interceptor_ref);

        self.layer_connector_ref
            .send(LocalMessage {
                message_id,
                inner: ProxyToLayerMessage::OutgoingConnect(Ok(OutgoingConnectResponse {
                    layer_address,
                    in_cluster_address: local_address,
                })),
            })
            .await;

        Ok(())
    }

    async fn handle_message(
        &mut self,
        message: OutgoingProxyMessage,
    ) -> Result<(), OutgoingProxyError> {
        match message {
            OutgoingProxyMessage::AgentDatagrams(msg) => match msg {
                DaemonUdpOutgoing::Close(id) => self.handle_agent_close(id, NetProtocol::Datagrams),
                DaemonUdpOutgoing::Connect(connect) => {
                    self.handle_agent_connect(connect, NetProtocol::Datagrams)
                        .await?
                }
                DaemonUdpOutgoing::Read(read) => {
                    self.handle_agent_read(read, NetProtocol::Datagrams).await
                }
            },
            OutgoingProxyMessage::AgentStreams(msg) => match msg {
                DaemonTcpOutgoing::Close(id) => self.handle_agent_close(id, NetProtocol::Stream),
                DaemonTcpOutgoing::Connect(connect) => {
                    self.handle_agent_connect(connect, NetProtocol::Stream)
                        .await?
                }
                DaemonTcpOutgoing::Read(read) => {
                    self.handle_agent_read(read, NetProtocol::Stream).await
                }
            },
            OutgoingProxyMessage::ConnectRequest(req) => {
                let OutgoingConnectRequest {
                    remote_address,
                    protocol,
                } = req.inner;

                self.queues.get(protocol).insert(req.message_id);

                self.agent_sender
                    .send(protocol.wrap_agent_connect(remote_address))
                    .await;
            }
        }

        Ok(())
    }

    async fn handle_interceptor_result(
        &mut self,
        result: ComponentResult<InterceptorId, io::Error>,
    ) {
        let id = match result {
            Ok(id) => {
                tracing::trace!("outgoing interceptor {id:?} finished");
                id
            }
            Err(ComponentError::Error(id, e)) => {
                tracing::error!("outgoing interceptor {id:?} failed with {e:?}");
                id
            }
            Err(ComponentError::Panic(id)) => {
                tracing::error!("outgoing interceptor {id:?} panicked");
                id
            }
        };

        if self.interceptors.remove(&id).is_some() {
            self.agent_sender
                .send(id.protocol.wrap_agent_close(id.connection_id))
                .await;
        }
    }
}

#[derive(Error, Debug)]
pub enum OutgoingProxyError {
    #[error("connection {0} not found")]
    NoConnectionId(ConnectionId),
    #[error("{0}")]
    RequestQueueEmpty(#[from] RequestQueueEmpty),
    #[error("failed to prepare interceptor: {0}")]
    InterceptorSetupFailed(#[from] io::Error),
}

#[derive(From)]
pub enum OutgoingProxyMessage {
    AgentStreams(DaemonTcpOutgoing),
    AgentDatagrams(DaemonUdpOutgoing),
    ConnectRequest(LocalMessage<OutgoingConnectRequest>),
}

impl Component for OutgoingProxy {
    type Id = &'static str;
    type Error = OutgoingProxyError;
    type Message = OutgoingProxyMessage;

    fn id(&self) -> Self::Id {
        "OUTGOING_PROXY"
    }

    async fn run(mut self, mut message_rx: Receiver<Self::Message>) -> Result<(), Self::Error> {
        loop {
            tokio::select! {
                message = message_rx.recv() => match message {
                    Some(message) => self.handle_message(message).await?,
                    None => break Ok(()),
                },

                Some(res) = self.system.next() => self.handle_interceptor_result(res).await,
            }
        }
    }
}
