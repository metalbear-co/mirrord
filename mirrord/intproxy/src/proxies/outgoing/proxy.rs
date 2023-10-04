use std::io;

use mirrord_protocol::{
    outgoing::{tcp::DaemonTcpOutgoing, udp::DaemonUdpOutgoing, DaemonConnect, DaemonRead},
    ConnectionId, RemoteResult,
};
use thiserror::Error;

use super::interceptor::{InterceptorId, OutgoingInterceptor, OutgoingInterceptorMessage};
use crate::{
    agent_conn::AgentSender,
    layer_conn::{LayerCommunicationError, LayerSender},
    protocol::{
        LocalMessage, NetProtocol, OutgoingConnectRequest, OutgoingConnectResponse,
        ProxyToLayerMessage,
    },
    request_queue::{RequestQueue, RequestQueueEmpty},
    task_manager::{MessageBus, Task, TaskManager, TaskMessageOut},
};

#[derive(Default)]
struct Queues {
    datagrams: RequestQueue,
    stream: RequestQueue,
}

impl Queues {
    fn get(&mut self, protocol: NetProtocol) -> &mut RequestQueue {
        match protocol {
            NetProtocol::Datagrams => &mut self.datagrams,
            NetProtocol::Stream => &mut self.stream,
        }
    }
}

pub struct OutgoingProxy {
    agent_sender: AgentSender,
    layer_sender: LayerSender,
    queues: Queues,
    interceptors: TaskManager<OutgoingInterceptor>,
}

impl OutgoingProxy {
    pub fn new(layer_sender: LayerSender, agent_sender: AgentSender) -> Self {
        Self {
            agent_sender,
            layer_sender,
            queues: Default::default(),
            interceptors: TaskManager::new(512),
        }
    }

    async fn handle_agent_close(&mut self, connection_id: ConnectionId, protocol: NetProtocol) {
        let res = self
            .interceptors
            .send(
                InterceptorId {
                    connection_id,
                    protocol,
                },
                OutgoingInterceptorMessage::AgentClosed,
            )
            .await;

        if res.is_err() {
            tracing::trace!(
                "agent sent message to closed interceptor, connection id {connection_id}"
            );
        }
    }

    async fn handle_agent_read(&self, read: RemoteResult<DaemonRead>, protocol: NetProtocol) {
        let Ok(DaemonRead {
            connection_id,
            bytes,
        }) = read
        else {
            return;
        };

        let res = self
            .interceptors
            .send(
                InterceptorId {
                    connection_id,
                    protocol,
                },
                OutgoingInterceptorMessage::Bytes(bytes),
            )
            .await;

        if res.is_err() {
            tracing::trace!(
                "agent sent message to closed interceptor, connection id {connection_id}"
            );
        }
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
                return self
                    .layer_sender
                    .send(LocalMessage {
                        message_id,
                        inner: ProxyToLayerMessage::OutgoingConnect(Err(e)),
                    })
                    .await
                    .map_err(Into::into);
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

        self.layer_sender
            .send(LocalMessage {
                message_id,
                inner: ProxyToLayerMessage::OutgoingConnect(Ok(OutgoingConnectResponse {
                    layer_address,
                    in_cluster_address: local_address,
                })),
            })
            .await?;

        Ok(())
    }

    async fn handle_agent_message(
        &mut self,
        message: OutgoingProxyMessage,
    ) -> Result<(), OutgoingProxyError> {
        match message {
            OutgoingProxyMessage::AgentDatagrams(msg) => match msg {
                DaemonUdpOutgoing::Close(id) => {
                    self.handle_agent_close(id, NetProtocol::Datagrams).await
                }
                DaemonUdpOutgoing::Connect(connect) => {
                    self.handle_agent_connect(connect, NetProtocol::Datagrams)
                        .await?
                }
                DaemonUdpOutgoing::Read(read) => {
                    self.handle_agent_read(read, NetProtocol::Datagrams).await
                }
            },
            OutgoingProxyMessage::AgentStreams(msg) => match msg {
                DaemonTcpOutgoing::Close(id) => {
                    self.handle_agent_close(id, NetProtocol::Stream).await
                }
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

    async fn handle_interceptor_message(
        &mut self,
        message: TaskMessageOut<OutgoingInterceptor>,
    ) -> Result<(), OutgoingProxyError> {
        todo!()
        // match result {
        //     Ok(()) => {
        //         tracing::trace!("outgoing interceptor {id} finished");
        //     }
        //     Err(e) => {
        //         tracing::error!("outgoing interceptor {id} failed: {e}");
        //     }
        // };

        // if self.interceptors.remove(&id).is_some() {
        //     self.agent_sender
        //         .send(id.protocol.wrap_agent_close(id.connection_id))
        //         .await;
        // }
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
    #[error("communication with layer failed: {0}")]
    LayerCommunicationError(#[from] LayerCommunicationError),
}

pub enum OutgoingProxyMessage {
    AgentStreams(DaemonTcpOutgoing),
    AgentDatagrams(DaemonUdpOutgoing),
    ConnectRequest(LocalMessage<OutgoingConnectRequest>),
}

impl Task for OutgoingProxy {
    type MessageOut = ();
    type Error = OutgoingProxyError;
    type Id = &'static str;
    type MessageIn = OutgoingProxyMessage;

    fn id(&self) -> Self::Id {
        "OUTGOING_PROXY"
    }

    async fn run(mut self, messages: &mut MessageBus<Self>) -> Result<(), Self::Error> {
        loop {
            // tokio::select! {
            //     message = messages.recv() => self.handle_agent_message(message).await?,
            //     message = self.interceptors.receive() =>
            // self.handle_interceptor_message(message).await?, }
        }
    }
}
