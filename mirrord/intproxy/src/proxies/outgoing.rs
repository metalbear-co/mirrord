//! Handles the logic of the `outgoing` feature.

use std::{collections::HashMap, fmt, io, time::Instant};

use bytes::Bytes;
use mirrord_intproxy_protocol::{
    LayerId, MessageId, NetProtocol, OutgoingConnectRequest, OutgoingConnectResponse,
    ProxyToLayerMessage,
};
use mirrord_protocol::{
    ConnectionId, DaemonMessage, RemoteResult, ResponseError,
    outgoing::{
        DaemonConnect, DaemonRead, SocketAddress, tcp::DaemonTcpOutgoing, udp::DaemonUdpOutgoing,
    },
};
use thiserror::Error;
use tracing::Level;

use self::interceptor::Interceptor;
use crate::{
    ProxyMessage,
    background_tasks::{
        BackgroundTask, BackgroundTasks, MessageBus, TaskError, TaskSender, TaskUpdate,
    },
    error::{UnexpectedAgentMessage, agent_lost_io_error},
    main_tasks::ToLayer,
    proxies::outgoing::{
        net_protocol_ext::{NetProtocolExt, PreparedSocket},
        non_blocking_hack::PreparedTcpSocket,
    },
    request_queue::RequestQueue,
};

mod interceptor;
mod net_protocol_ext;
mod non_blocking_hack;

/// Errors that can occur when handling the `outgoing` feature.
#[derive(Error, Debug)]
pub enum OutgoingProxyError {
    /// The agent sent an error not bound to any [`ConnectionId`].
    /// This is assumed to be a general agent error.
    /// Originates only from the [`RemoteResult<DaemonRead>`] message.
    #[error("agent error: {0}")]
    ResponseError(#[from] ResponseError),
    /// The agent sent a [`DaemonConnect`] response, but the [`RequestQueue`] for layer's connec
    /// requests was empty. This should never happen.
    #[error(transparent)]
    UnexpectedAgentMessage(#[from] UnexpectedAgentMessage),
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

/// Lightweight (no allocations) [`ProxyMessage`] to be returned when connection with the
/// mirrord-agent is lost. Must be converted into real [`ProxyMessage`] via [`From`].
pub struct AgentLostOutgoingResponse(LayerId, MessageId);

impl From<AgentLostOutgoingResponse> for ToLayer {
    fn from(value: AgentLostOutgoingResponse) -> Self {
        let AgentLostOutgoingResponse(layer_id, message_id) = value;
        let error = agent_lost_io_error();

        ToLayer {
            layer_id,
            message_id,
            message: ProxyToLayerMessage::OutgoingConnect(Err(error)),
        }
    }
}

#[derive(Debug)]
struct ConnectInProgress {
    prepared_socket: Option<PreparedTcpSocket>,
    remote_address: SocketAddress,
    requested_at: Instant,
}

/// Handles logic and state of the `outgoing` feature.
///
/// Run as a [`BackgroundTask`].
///
/// # Standard flow
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
///
/// # TCP non blocking flow
///
/// Enabled with a flag.
///
/// TODO
pub struct OutgoingProxy {
    /// For [`OutgoingConnectRequest`]s related to [`NetProtocol::Datagrams`].
    datagrams_reqs: RequestQueue<ConnectInProgress>,
    /// For [`OutgoingConnectRequest`]s related to [`NetProtocol::Stream`].
    stream_reqs: RequestQueue<ConnectInProgress>,
    /// [`TaskSender`]s for active [`Interceptor`] tasks.
    txs: HashMap<InterceptorId, TaskSender<Interceptor>>,
    /// For managing [`Interceptor`] tasks.
    background_tasks: BackgroundTasks<InterceptorId, Bytes, io::Error>,
    /// Whether TCP connect requests should be handled in a non-blocking way.
    ///
    /// See struct level docs for more info.
    non_blocking_tcp_connect: bool,
}

impl OutgoingProxy {
    /// Used when registering new [`Interceptor`] tasks in the [`BackgroundTasks`] struct.
    const CHANNEL_SIZE: usize = 512;

    /// Creates a new instance, ready to run.
    ///
    /// # Params
    ///
    /// * `non_blocking_tcp_connect` - see struct level docs
    pub fn new(non_blocking_tcp_connect: bool) -> Self {
        if non_blocking_tcp_connect {
            tokio::spawn(non_blocking_hack::working_method());
        }

        Self {
            datagrams_reqs: Default::default(),
            stream_reqs: Default::default(),
            txs: Default::default(),
            background_tasks: Default::default(),
            non_blocking_tcp_connect,
        }
    }

    /// Retrieves correct [`RequestQueue`] for the given [`NetProtocol`].
    fn queue(&mut self, protocol: NetProtocol) -> &mut RequestQueue<ConnectInProgress> {
        match protocol {
            NetProtocol::Datagrams => &mut self.datagrams_reqs,
            NetProtocol::Stream => &mut self.stream_reqs,
        }
    }

    /// Passes the data to the correct [`Interceptor`] task.
    /// Fails when the agent sends an error, because this error cannot be traced back to an exact
    /// connection.
    #[tracing::instrument(level = Level::TRACE, skip(self))]
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

        interceptor.send(bytes.0).await;

        Ok(())
    }

    /// Handles agent's response to a connection request.
    /// Prepares a local socket and registers a new [`Interceptor`] task for this connection.
    /// Replies to the layer's request.
    #[tracing::instrument(level = Level::DEBUG, skip(self, message_bus), ret, err)]
    async fn handle_connect_response(
        &mut self,
        connect: RemoteResult<DaemonConnect>,
        protocol: NetProtocol,
        message_bus: &mut MessageBus<Self>,
    ) -> Result<(), OutgoingProxyError> {
        let (message_id, layer_id, in_progress) =
            self.queue(protocol).pop_front_with_data().ok_or_else(|| {
                let message = match protocol {
                    NetProtocol::Datagrams => {
                        DaemonMessage::UdpOutgoing(DaemonUdpOutgoing::Connect(connect.clone()))
                    }
                    NetProtocol::Stream => {
                        DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Connect(connect.clone()))
                    }
                };
                UnexpectedAgentMessage(message)
            })?;

        let DaemonConnect {
            connection_id,
            remote_address,
            local_address,
        } = match connect {
            Ok(connect) => {
                tracing::info!(
                    address = %in_progress.remote_address,
                    elapsed = ?in_progress.requested_at.elapsed(),
                    "Outgoing connect request succeeded",
                );
                connect
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    address = %in_progress.remote_address,
                    elapsed = ?in_progress.requested_at.elapsed(),
                    "Outgoing connect request failed",
                );

                if in_progress.prepared_socket.is_none() {
                    message_bus
                        .send(ToLayer {
                            message: ProxyToLayerMessage::OutgoingConnect(Err(error)),
                            message_id,
                            layer_id,
                        })
                        .await;
                }

                return Ok(());
            }
        };

        let prepared_socket = match in_progress.prepared_socket {
            Some(socket) => PreparedSocket::TcpNonBlocking(socket),
            None => {
                let prepared_socket = protocol.prepare_socket(remote_address).await?;
                let layer_address = prepared_socket.local_address()?;

                message_bus
                    .send(ToLayer {
                        message: ProxyToLayerMessage::OutgoingConnect(Ok(
                            OutgoingConnectResponse {
                                layer_address,
                                in_cluster_address: Some(local_address),
                            },
                        )),
                        message_id,
                        layer_id,
                    })
                    .await;

                prepared_socket
            }
        };

        let id = InterceptorId {
            connection_id,
            protocol,
        };

        tracing::debug!(
            %id,
            remote_address = %in_progress.remote_address,
            "Starting interceptor task"
        );
        let interceptor = self.background_tasks.register(
            Interceptor::new(id, prepared_socket),
            id,
            Self::CHANNEL_SIZE,
        );
        self.txs.insert(id, interceptor);

        Ok(())
    }

    /// Saves the layer's request id and sends the connection request to the agent.
    #[tracing::instrument(level = Level::DEBUG, skip(self, message_bus), ret)]
    async fn handle_connect_request(
        &mut self,
        message_id: MessageId,
        session_id: LayerId,
        request: OutgoingConnectRequest,
        message_bus: &mut MessageBus<Self>,
    ) -> Result<(), OutgoingProxyError> {
        let prepared_socket = if self.non_blocking_tcp_connect
            && matches!(&request.remote_address, SocketAddress::Ip(..))
            && request.protocol == NetProtocol::Stream
        {
            if let Some(method) = non_blocking_hack::working_method().await {
                let ipv4 = matches!(&request.remote_address, SocketAddress::Ip(ip) if ip.is_ipv4());
                Some(method.prepare_socket(ipv4).await?)
            } else {
                tracing::warn!(
                    remote_address = %request.remote_address,
                    "Cannot emulate non-blocking TCP connect, no working hack detected."
                );
                None
            }
        } else {
            None
        };

        if let Some(socket) = &prepared_socket {
            let addr = socket.local_addr()?;
            let to_layer = ToLayer {
                message_id,
                layer_id: session_id,
                message: ProxyToLayerMessage::OutgoingConnect(Ok(OutgoingConnectResponse {
                    layer_address: addr.into(),
                    in_cluster_address: None,
                })),
            };
            message_bus.send(to_layer).await;
        }

        self.queue(request.protocol).push_back_with_data(
            message_id,
            session_id,
            ConnectInProgress {
                prepared_socket,
                remote_address: request.remote_address.clone(),
                requested_at: Instant::now(),
            },
        );

        let msg = request.protocol.wrap_agent_connect(request.remote_address);
        message_bus.send(ProxyMessage::ToAgent(msg)).await;

        Ok(())
    }

    #[tracing::instrument(level = Level::INFO, skip_all, ret)]
    async fn handle_connection_refresh(&mut self, message_bus: &mut MessageBus<Self>) {
        tracing::debug!("Closing all local connections");
        self.txs.clear();
        self.background_tasks.clear();

        tracing::debug!(
            responses = self.datagrams_reqs.len(),
            "Flushing error responses to UDP connect requests"
        );
        while let Some((message_id, layer_id)) = self.datagrams_reqs.pop_front() {
            message_bus
                .send(ToLayer::from(AgentLostOutgoingResponse(
                    layer_id, message_id,
                )))
                .await;
        }

        tracing::debug!(
            responses = self.stream_reqs.len(),
            "Flushing error responses to TCP connect requests"
        );
        while let Some((message_id, layer_id)) = self.stream_reqs.pop_front() {
            message_bus
                .send(ToLayer::from(AgentLostOutgoingResponse(
                    layer_id, message_id,
                )))
                .await;
        }
    }
}

/// Messages consumed by the [`OutgoingProxy`] running as a [`BackgroundTask`].
pub enum OutgoingProxyMessage {
    AgentStream(DaemonTcpOutgoing),
    AgentDatagrams(DaemonUdpOutgoing),
    LayerConnect(OutgoingConnectRequest, MessageId, LayerId),
    ConnectionRefresh,
}

impl BackgroundTask for OutgoingProxy {
    type Error = OutgoingProxyError;
    type MessageIn = OutgoingProxyMessage;
    type MessageOut = ProxyMessage;

    #[tracing::instrument(level = Level::INFO, name = "outgoing_proxy_main_loop", skip_all, ret, err)]
    async fn run(&mut self, message_bus: &mut MessageBus<Self>) -> Result<(), Self::Error> {
        loop {
            tokio::select! {
                msg = message_bus.recv() => match msg {
                    None => {
                        tracing::debug!("Message bus closed, exiting");
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
                    ).await?,
                    Some(OutgoingProxyMessage::ConnectionRefresh) => self.handle_connection_refresh(message_bus).await,
                },

                Some(task_update) = self.background_tasks.next() => match task_update {
                    (id, TaskUpdate::Message(bytes)) => {
                        let msg = id.protocol.wrap_agent_write(id.connection_id, bytes);
                        message_bus.send(ProxyMessage::ToAgent(msg)).await;
                    }
                    (id, TaskUpdate::Finished(res)) => {
                        match res {
                            Ok(()) => tracing::debug!(%id, "Interceptor finished"),
                            Err(TaskError::Error(error)) => {
                                tracing::warn!(%id, %error, "Interceptor failed");
                            }
                            Err(TaskError::Panic) => {
                                tracing::error!(%id, "Interceptor panicked");
                            }
                        }

                        if self.txs.remove(&id).is_some() {
                            tracing::trace!(%id, "Local connection closed, notifying the agent");
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

#[cfg(test)]
mod test {
    use std::net::SocketAddr;

    use mirrord_intproxy_protocol::{
        LayerId, NetProtocol, OutgoingConnectRequest, ProxyToLayerMessage,
    };
    use mirrord_protocol::{
        ClientMessage,
        outgoing::{
            DaemonConnect, LayerConnect, SocketAddress,
            tcp::{DaemonTcpOutgoing, LayerTcpOutgoing},
        },
    };

    use crate::{
        background_tasks::{BackgroundTasks, TaskUpdate},
        main_tasks::{ProxyMessage, ToLayer},
        proxies::outgoing::{OutgoingProxy, OutgoingProxyError, OutgoingProxyMessage},
    };

    /// Verifies that the outgoing proxy can handle operator reconnect
    /// when there is an open connection.
    #[tokio::test]
    async fn clear_on_reconnect() {
        let peer_addr = "1.1.1.1:80".parse::<SocketAddr>().unwrap();

        let mut background_tasks: BackgroundTasks<(), ProxyMessage, OutgoingProxyError> =
            BackgroundTasks::default();
        let outgoing = background_tasks.register(OutgoingProxy::new(false), (), 8);

        for i in 0..=1 {
            // Layer wants to make an outgoing connection.
            outgoing
                .send(OutgoingProxyMessage::LayerConnect(
                    OutgoingConnectRequest {
                        remote_address: SocketAddress::Ip(peer_addr),
                        protocol: NetProtocol::Stream,
                    },
                    i,
                    LayerId(0),
                ))
                .await;
            let message = background_tasks.next().await.unwrap().1.unwrap_message();
            assert_eq!(
                message,
                ProxyMessage::ToAgent(ClientMessage::TcpOutgoing(LayerTcpOutgoing::Connect(
                    LayerConnect {
                        remote_address: SocketAddress::Ip(peer_addr),
                    }
                ))),
            );

            // Operator confirms with connection id 0.
            outgoing
                .send(OutgoingProxyMessage::AgentStream(
                    DaemonTcpOutgoing::Connect(Ok(DaemonConnect {
                        connection_id: 0,
                        remote_address: SocketAddress::Ip(peer_addr),
                        local_address: SocketAddress::Ip("127.0.0.1:1337".parse().unwrap()),
                    })),
                ))
                .await;
            let message = background_tasks.next().await.unwrap().1.unwrap_message();
            match message {
                ProxyMessage::ToLayer(ToLayer {
                    message_id,
                    layer_id: LayerId(0),
                    message: ProxyToLayerMessage::OutgoingConnect(Ok(..)),
                }) => {
                    assert_eq!(message_id, i);
                }
                other => panic!("unexpected message from outgoing proxy: {other:?}"),
            }

            // Connection with the operator was reset.
            outgoing.send(OutgoingProxyMessage::ConnectionRefresh).await;
        }

        std::mem::drop(outgoing);
        match background_tasks.next().await.unwrap() {
            ((), TaskUpdate::Finished(Ok(()))) => {}
            other => panic!("unexpected update from the outgoing proxy: {other:?}"),
        }
    }
}
