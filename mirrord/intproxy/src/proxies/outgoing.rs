//! Handles the logic of the `outgoing` feature.

use std::{collections::HashMap, fmt, io};

use mirrord_protocol::{
    outgoing::{tcp::DaemonTcpOutgoing, udp::DaemonUdpOutgoing, DaemonConnect, DaemonRead},
    ConnectionId, RemoteResult, ResponseError,
};
use thiserror::Error;

use self::interceptor::Interceptor;
use crate::{
    background_tasks::{BackgroundTask, BackgroundTasks, MessageBus, TaskSender, TaskUpdate},
    main_tasks::ToLayer,
    protocol::{
        LayerId, MessageId, NetProtocol, OutgoingConnectRequest, OutgoingConnectResponse,
        ProxyToLayerMessage,
    },
    request_queue::{RequestQueue, RequestQueueEmpty},
    ProxyMessage,
};

mod interceptor;
mod protocols;

/// Errors that can occur when handling the `outgoing` feature.
#[derive(Error, Debug)]
pub enum OutgoingProxyError {
    /// The agent sent an error not bound to any [`ConnectionId`](mirrord_protocol::ConnectionId).
    /// This is assumed to be a general agent error.
    /// Originates only from the [`RemoteResult<DaemonRead>`] message.
    #[error("agent error: {0}")]
    ResponseError(#[from] ResponseError),
    /// The agent sent a [`DaemonConnect`] response, but the [`RequestQueue`] for layer's connec
    /// requests was empty. This should never happen.
    #[error("failed to match connect response: {0}")]
    RequestQueueEmpty(#[from] RequestQueueEmpty),
    /// The proxy failed to prepare a new local socket for the intercepted connection.
    #[error("failed to prepare local socket: {0}")]
    SocketSetupError(#[from] io::Error),
}

/// Id of a single [`Interceptor`] task.
/// Used to manage [`Interceptor`]s with the [`BackgroundTasks`] struct.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct InterceptorId {
    /// Id of the intercepted connection.
    pub connection_id: ConnectionId,
    /// Network protocol used.
    pub protocol: NetProtocol,
}

impl fmt::Display for InterceptorId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "outgoing interceptor {}-{}",
            self.connection_id, self.protocol
        )
    }
}

/// Handles logic and state of the `outgoing` feature.
/// Run as a [`BackgroundTask`].
///
/// # Flow
///
/// 1. Proxy receives an [`OutgoingConnectRequest`] from the layer and sends a corresponding
///    [`LayerConnect`](mirrord_protocol::outgoing::LayerConnect) to the agent.
/// 2. Proxy receives a confirmation from the agent.
/// 3. Proxy creates a new socket and starts a new outgoing [`Interceptor`] background task to
///    manage it.
/// 4. Proxy sends a confirmation to the layer.
/// 5. The layer connects to the socket managed by the [`Interceptor`] task.
/// 6. The proxy passes the data between the agent and the [`Interceptor`] task.
/// 7. If the layer closes the connection, the [`Interceptor`] exits and the proxy notifies the
///    agent. If the agent closes the connection, the proxy shuts down the [`Interceptor`].
pub struct OutgoingProxy {
    /// For [`OutgoingConnectRequest`]s related to [`NetProtocol::Datagrams`].
    datagrams_reqs: RequestQueue,
    /// For [`OutgoingConnectRequest`]s related to [`NetProtocol::Stream`].
    stream_reqs: RequestQueue,
    /// [`TaskSender`]s for active [`Interceptor`] tasks.
    txs: HashMap<InterceptorId, TaskSender<Interceptor>>,
    /// For managing [`Interceptor`] tasks.
    background_tasks: BackgroundTasks<InterceptorId, Vec<u8>, io::Error>,
}

impl Default for OutgoingProxy {
    fn default() -> Self {
        Self {
            datagrams_reqs: Default::default(),
            stream_reqs: Default::default(),
            txs: Default::default(),
            background_tasks: Default::default(),
        }
    }
}

impl OutgoingProxy {
    /// Used when registering new [`Interceptor`] tasks in the [`BackgroundTasks`] struct.
    const CHANNEL_SIZE: usize = 512;

    /// Retrieves correct [`RequestQueue`] for the given [`NetProtocol`].
    fn queue(&mut self, protocol: NetProtocol) -> &mut RequestQueue {
        match protocol {
            NetProtocol::Datagrams => &mut self.datagrams_reqs,
            NetProtocol::Stream => &mut self.stream_reqs,
        }
    }

    /// Passes the data to the correct [`Interceptor`] task.
    /// Fails when the agent sends an error, because this error cannot be traced back to an exact
    /// connection.
    #[tracing::instrument(level = "trace", skip(self))]
    async fn handle_agent_read(
        &mut self,
        read: RemoteResult<DaemonRead>,
        protocol: NetProtocol,
    ) -> Result<(), OutgoingProxyError> {
        let DaemonRead {
            connection_id,
            bytes,
        } = read?;

        let id = InterceptorId {
            connection_id,
            protocol,
        };

        let Some(interceptor) = self.txs.get(&id) else {
            tracing::trace!(
                "{id} does not exist, received data for connection that is already closed"
            );
            return Ok(());
        };

        interceptor.send(bytes).await;

        Ok(())
    }

    /// Handles agent's response to a connection request.
    /// Prepares a local socket and registers a new [`Interceptor`] task for this connection.
    /// Replies to the layer's request.
    #[tracing::instrument(level = "trace", skip(self, message_bus))]
    async fn handle_connect_response(
        &mut self,
        connect: RemoteResult<DaemonConnect>,
        protocol: NetProtocol,
        message_bus: &mut MessageBus<Self>,
    ) -> Result<(), OutgoingProxyError> {
        let (message_id, layer_id) = self.queue(protocol).get()?;

        let connect = match connect {
            Ok(connect) => connect,
            Err(e) => {
                message_bus
                    .send(ToLayer {
                        message: ProxyToLayerMessage::OutgoingConnect(Err(e)),
                        message_id,
                        layer_id,
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

        let id = InterceptorId {
            connection_id,
            protocol,
        };

        let interceptor = self.background_tasks.register(
            Interceptor::new(prepared_socket),
            id,
            Self::CHANNEL_SIZE,
        );
        self.txs.insert(id, interceptor);

        message_bus
            .send(ToLayer {
                message: ProxyToLayerMessage::OutgoingConnect(Ok(OutgoingConnectResponse {
                    layer_address,
                    in_cluster_address: local_address,
                })),
                message_id,
                layer_id,
            })
            .await;

        Ok(())
    }

    /// Saves the layer's request id and sends the connection request to the agent.
    #[tracing::instrument(level = "trace", skip(self, message_bus))]
    async fn handle_connect_request(
        &mut self,
        message_id: MessageId,
        session_id: LayerId,
        request: OutgoingConnectRequest,
        message_bus: &mut MessageBus<Self>,
    ) {
        self.queue(request.protocol).insert(message_id, session_id);

        let msg = request.protocol.wrap_agent_connect(request.remote_address);
        message_bus.send(ProxyMessage::ToAgent(msg)).await;
    }
}

/// Messages consumed by the [`OutgoingProxy`] running as a [`BackgroundTask`].
pub enum OutgoingProxyMessage {
    AgentStream(DaemonTcpOutgoing),
    AgentDatagrams(DaemonUdpOutgoing),
    LayerConnect(OutgoingConnectRequest, MessageId, LayerId),
}

impl BackgroundTask for OutgoingProxy {
    type Error = OutgoingProxyError;
    type MessageIn = OutgoingProxyMessage;
    type MessageOut = ProxyMessage;

    async fn run(mut self, message_bus: &mut MessageBus<Self>) -> Result<(), Self::Error> {
        loop {
            tokio::select! {
                msg = message_bus.recv() => match msg {
                    None => {
                        tracing::trace!("message bus closed, exiting");
                        break Ok(());
                    },
                    Some(OutgoingProxyMessage::AgentStream(req)) => match req {
                        DaemonTcpOutgoing::Close(close) => {
                            let id = InterceptorId { connection_id: close, protocol: NetProtocol::Stream};
                            self.txs.remove(&id);
                        },
                        DaemonTcpOutgoing::Read(read) => self.handle_agent_read(read, NetProtocol::Stream).await?,
                        DaemonTcpOutgoing::Connect(connect) => self.handle_connect_response(connect, NetProtocol::Stream, message_bus).await?,
                    }
                    Some(OutgoingProxyMessage::AgentDatagrams(req)) => match req {
                        DaemonUdpOutgoing::Close(close) => {
                            let id = InterceptorId { connection_id: close, protocol: NetProtocol::Datagrams};
                            self.txs.remove(&id);
                        }
                        DaemonUdpOutgoing::Read(read) => self.handle_agent_read(read, NetProtocol::Datagrams).await?,
                        DaemonUdpOutgoing::Connect(connect) => self.handle_connect_response(connect, NetProtocol::Datagrams, message_bus).await?,
                    }
                    Some(OutgoingProxyMessage::LayerConnect(req, message_id, session_id)) => self.handle_connect_request(
                        message_id,
                        session_id,
                        req,
                        message_bus
                    ).await,
                },

                Some(task_update) = self.background_tasks.next() => match task_update {
                    (id, TaskUpdate::Message(bytes)) => {
                        let msg = id.protocol.wrap_agent_write(id.connection_id, bytes);
                        message_bus.send(ProxyMessage::ToAgent(msg)).await;
                    }
                    (id, TaskUpdate::Finished(res)) => {
                        tracing::trace!("{id} finished: {res:?}");

                        if self.txs.remove(&id).is_some() {
                            tracing::trace!("local connection closed, notifying the agent");
                            let msg = id.protocol.wrap_agent_close(id.connection_id);
                            let _ = message_bus.send(ProxyMessage::ToAgent(msg)).await;
                            self.txs.remove(&id);
                        }
                    }
                },
            }
        }
    }
}
