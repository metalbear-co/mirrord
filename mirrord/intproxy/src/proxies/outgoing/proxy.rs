use std::io;

use mirrord_protocol::{
    outgoing::{tcp::DaemonTcpOutgoing, udp::DaemonUdpOutgoing, DaemonConnect, DaemonRead},
    ClientMessage, ConnectionId, RemoteResult,
};
use thiserror::Error;

use super::interceptor::{
    InterceptorId, OutgoingInterceptor, OutgoingInterceptorIn, OutgoingInterceptorOut,
};
use crate::{
    protocol::{
        LocalMessage, NetProtocol, OutgoingConnectRequest, OutgoingConnectResponse,
        ProxyToLayerMessage,
    },
    request_queue::{RequestQueue, RequestQueueEmpty},
    task_manager::{MessageBus, Task, TaskManager, TaskMessageOut},
};

pub struct OutgoingProxy {
    datagrams_reqs: RequestQueue,
    stream_reqs: RequestQueue,
    interceptors: TaskManager<OutgoingInterceptor>,
}

impl Default for OutgoingProxy {
    fn default() -> Self {
        Self {
            datagrams_reqs: Default::default(),
            stream_reqs: Default::default(),
            interceptors: TaskManager::new(512),
        }
    }
}

impl OutgoingProxy {
    fn queue(&mut self, protocol: NetProtocol) -> &mut RequestQueue {
        match protocol {
            NetProtocol::Datagrams => &mut self.datagrams_reqs,
            NetProtocol::Stream => &mut self.stream_reqs,
        }
    }

    async fn handle_agent_close(&mut self, connection_id: ConnectionId, protocol: NetProtocol) {
        self.interceptors
            .send(
                InterceptorId {
                    connection_id,
                    protocol,
                },
                OutgoingInterceptorIn::AgentClosed,
            )
            .await
    }

    async fn handle_agent_read(&self, read: RemoteResult<DaemonRead>, protocol: NetProtocol) {
        let Ok(DaemonRead {
            connection_id,
            bytes,
        }) = read
        else {
            return;
        };

        self.interceptors
            .send(
                InterceptorId {
                    connection_id,
                    protocol,
                },
                OutgoingInterceptorIn::Bytes(bytes),
            )
            .await
    }

    async fn handle_agent_connect(
        &mut self,
        connect: RemoteResult<DaemonConnect>,
        protocol: NetProtocol,
    ) -> Result<OutgoingProxyOut, OutgoingProxyError> {
        let message_id = self.queue(protocol).get()?;

        let connect = match connect {
            Ok(connect) => connect,
            Err(e) => {
                return Ok(OutgoingProxyOut::ToLayer(LocalMessage {
                    message_id,
                    inner: ProxyToLayerMessage::OutgoingConnect(Err(e)),
                }))
            }
        };

        let DaemonConnect {
            connection_id,
            remote_address,
            local_address,
        } = connect;

        let prepared_socket = protocol.prepare_socket(remote_address).await?;
        let layer_address = prepared_socket.local_address()?;

        let interceptor = OutgoingInterceptor::new(connection_id, protocol, prepared_socket);
        self.interceptors.spawn(interceptor);

        Ok(OutgoingProxyOut::ToLayer(LocalMessage {
            message_id,
            inner: ProxyToLayerMessage::OutgoingConnect(Ok(OutgoingConnectResponse {
                layer_address,
                in_cluster_address: local_address,
            })),
        }))
    }

    async fn handle_message(
        &mut self,
        message: OutgoingProxyIn,
    ) -> Result<Option<OutgoingProxyOut>, OutgoingProxyError> {
        let out = match message {
            OutgoingProxyIn::AgentDatagrams(msg) => match msg {
                DaemonUdpOutgoing::Close(id) => {
                    self.handle_agent_close(id, NetProtocol::Datagrams).await;
                    None
                }
                DaemonUdpOutgoing::Connect(connect) => Some(
                    self.handle_agent_connect(connect, NetProtocol::Datagrams)
                        .await?,
                ),
                DaemonUdpOutgoing::Read(read) => {
                    self.handle_agent_read(read, NetProtocol::Datagrams).await;
                    None
                }
            },

            OutgoingProxyIn::AgentStreams(msg) => match msg {
                DaemonTcpOutgoing::Close(id) => {
                    self.handle_agent_close(id, NetProtocol::Stream).await;
                    None
                }
                DaemonTcpOutgoing::Connect(connect) => Some(
                    self.handle_agent_connect(connect, NetProtocol::Stream)
                        .await?,
                ),
                DaemonTcpOutgoing::Read(read) => {
                    self.handle_agent_read(read, NetProtocol::Stream).await;
                    None
                }
            },

            OutgoingProxyIn::ConnectRequest(req) => {
                let OutgoingConnectRequest {
                    remote_address,
                    protocol,
                } = req.inner;

                self.queue(protocol).insert(req.message_id);

                Some(OutgoingProxyOut::ToAgent(
                    protocol.wrap_agent_connect(remote_address),
                ))
            }
        };

        Ok(out)
    }

    fn handle_interceptor_message(
        &mut self,
        message: TaskMessageOut<OutgoingInterceptor>,
    ) -> Option<OutgoingProxyOut> {
        match message {
            TaskMessageOut::Raw { id, inner } => match inner {
                OutgoingInterceptorOut::Bytes(bytes) => {
                    let message = id.protocol.wrap_agent_write(id.connection_id, bytes);
                    Some(OutgoingProxyOut::ToAgent(message))
                }
                OutgoingInterceptorOut::LayerClosed => {
                    let message = id.protocol.wrap_agent_close(id.connection_id);
                    Some(OutgoingProxyOut::ToAgent(message))
                }
            },
            TaskMessageOut::Result { id, inner } => match inner {
                Ok(()) => {
                    tracing::trace!("interceptor task {id:?} finished");
                    None
                }
                Err(e) => {
                    tracing::warn!("interceptor task {id:?} failed: {e}");
                    Some(OutgoingProxyOut::ToAgent(
                        id.protocol.wrap_agent_close(id.connection_id),
                    ))
                }
            },
        }
    }
}

#[derive(Error, Debug)]
pub enum OutgoingProxyError {
    #[error("{0}")]
    RequestQueueEmpty(#[from] RequestQueueEmpty),
    #[error("failed to prepare interceptor: {0}")]
    InterceptorSetupFailed(#[from] io::Error),
}

pub enum OutgoingProxyIn {
    AgentStreams(DaemonTcpOutgoing),
    AgentDatagrams(DaemonUdpOutgoing),
    ConnectRequest(LocalMessage<OutgoingConnectRequest>),
}

pub enum OutgoingProxyOut {
    ToLayer(LocalMessage<ProxyToLayerMessage>),
    ToAgent(ClientMessage),
}

impl Task for OutgoingProxy {
    type MessageOut = OutgoingProxyOut;
    type MessageIn = OutgoingProxyIn;
    type Error = OutgoingProxyError;
    type Id = &'static str;

    fn id(&self) -> Self::Id {
        "OUTGOING_PROXY"
    }

    async fn run(mut self, messages: &mut MessageBus<Self>) -> Result<(), Self::Error> {
        loop {
            let out = tokio::select! {
                message = messages.recv() => match message {
                    None => break Ok(()),
                    Some(message) => self.handle_message(message).await?,
                },

                message = self.interceptors.receive() => self.handle_interceptor_message(message),
            };

            if let Some(message) = out {
                messages.send(message).await;
            }
        }
    }
}
