//! Handles the logic of the `outgoing` feature.

use std::{collections::HashMap, fmt, io};

use bytes::Bytes;
use mirrord_intproxy_protocol::{
    LayerId, MessageId, NetProtocol, OutgoingConnectRequest, OutgoingConnectResponse,
    ProxyToLayerMessage, SocketMetadataResponse,
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
    error::UnexpectedAgentMessage,
    local_sockets::{LocalSocketEntry, LocalSockets},
    main_tasks::ToLayer,
    proxies::outgoing::net_protocol_ext::{NetProtocolExt, PreparedSocket},
    request_queue::RequestQueue,
};

mod interceptor;
mod net_protocol_ext;

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
    #[error("failed to prepare a local socket: {0}")]
    SocketSetupError(#[from] io::Error),
}

/// Id of a single [`Interceptor`] task.
///
/// Used to manage [`Interceptor`]s with the [`BackgroundTasks`] struct.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct InterceptorId {
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

#[derive(Debug)]
struct ConnectionInProgress {
    local_socket: PreparedSocket,
    entry_guard: LocalSocketEntry,
    remote_address: SocketAddress,
}

/// Handles logic and state of the `outgoing` feature.
///
/// Run as one of the main [`BackgroundTask`]s of the internal proxy.
///
/// # Flow
///
/// 1. Receive [`OutgoingConnectRequest`] from the layer.
/// 2. Prepare a local socket (e.g. TPC listener), and send its address to the layer. After this
///    step, user app can use the socket for connect attemps (blocking or non-blocking).
/// 3. Send connect request to the agent.
/// 4. Receive response from the agent. Drop the socket on error, start an [`Interceptor`] on
///    success.
/// 5. Layer connects to the socket.
/// 6. Proxy data between the agent and the [`Interceptor`] task.
/// 7. If the layer closes the connection, the [`Interceptor`] exits. Notify the agent. If the agent
///    closes the connection, the shut down the [`Interceptor`].
pub struct OutgoingProxy {
    /// In progress remote connect requests for [`NetProtocol::Datagrams`].
    datagrams_reqs: RequestQueue<ConnectionInProgress>,
    /// In progress remote connect requests for [`NetProtocol::Stream`].
    stream_reqs: RequestQueue<ConnectionInProgress>,
    /// [`TaskSender`]s for active [`Interceptor`] tasks.
    txs: HashMap<InterceptorId, TaskSender<Interceptor>>,
    /// For managing [`Interceptor`] tasks.
    background_tasks: BackgroundTasks<InterceptorId, Bytes, io::Error>,
    /// Stores remote addresses corresponding to local sockets.
    local_sockets: LocalSockets,
}

impl OutgoingProxy {
    /// Used when registering new [`Interceptor`] tasks in the [`BackgroundTasks`] struct.
    const CHANNEL_SIZE: usize = 512;

    pub fn new(local_sockets: LocalSockets) -> Self {
        Self {
            datagrams_reqs: Default::default(),
            stream_reqs: Default::default(),
            txs: Default::default(),
            background_tasks: Default::default(),
            local_sockets,
        }
    }

    /// Retrieves correct [`RequestQueue`] for the given [`NetProtocol`].
    fn queue(&mut self, protocol: NetProtocol) -> &mut RequestQueue<ConnectionInProgress> {
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
    #[tracing::instrument(level = Level::DEBUG, skip(self), ret, err)]
    fn handle_connect_response(
        &mut self,
        connect: RemoteResult<DaemonConnect>,
        protocol: NetProtocol,
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

        let connect = match connect {
            Ok(connect) => connect,
            Err(error) => {
                tracing::warn!(
                    %error,
                    message_id,
                    layer_id = layer_id.0,
                    remote_address = %in_progress.remote_address,
                    "Outgoing connect request failed"
                );
                return Ok(());
            }
        };

        let DaemonConnect {
            connection_id,
            local_address,
            ..
        } = connect;
        in_progress
            .entry_guard
            .modify(|response| response.agent_address = local_address);

        let id = InterceptorId {
            connection_id,
            protocol,
        };
        tracing::debug!(%id, "Starting interceptor task");
        let interceptor = self.background_tasks.register(
            Interceptor::new(id, in_progress.local_socket, in_progress.entry_guard),
            id,
            Self::CHANNEL_SIZE,
        );
        self.txs.insert(id, interceptor);

        Ok(())
    }

    /// Saves the layer's request id and sends the connection request to the agent.
    #[tracing::instrument(level = Level::DEBUG, skip(self, message_bus), ret, err)]
    async fn handle_connect_request(
        &mut self,
        message_id: MessageId,
        session_id: LayerId,
        request: OutgoingConnectRequest,
        message_bus: &mut MessageBus<Self>,
    ) -> Result<(), OutgoingProxyError> {
        let local_socket = request
            .protocol
            .prepare_socket(&request.remote_address)
            .await?;
        let layer_address = local_socket.local_address()?;
        let entry_guard = self.local_sockets.insert(
            layer_address.clone(),
            SocketMetadataResponse {
                agent_address: layer_address.clone(),
                peer_address: request.remote_address.clone(),
            },
        );
        self.queue(request.protocol).push_back_with_data(
            message_id,
            session_id,
            ConnectionInProgress {
                local_socket,
                entry_guard,
                remote_address: request.remote_address.clone(),
            },
        );

        let to_layer = ToLayer {
            layer_id: session_id,
            message_id,
            message: ProxyToLayerMessage::OutgoingConnect(Ok(OutgoingConnectResponse {
                layer_address,
            })),
        };
        message_bus.send(to_layer).await;

        let to_agent = request.protocol.wrap_agent_connect(request.remote_address);
        message_bus.send(to_agent).await;

        Ok(())
    }

    #[tracing::instrument(level = Level::INFO, skip_all, ret)]
    fn handle_connection_refresh(&mut self) {
        tracing::debug!("Closing all local connections");
        self.txs = Default::default();
        self.background_tasks = Default::default();
        self.datagrams_reqs = Default::default();
        self.stream_reqs = Default::default();
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
                        DaemonTcpOutgoing::Connect(connect) => self.handle_connect_response(connect, NetProtocol::Stream)?,
                    }
                    Some(OutgoingProxyMessage::AgentDatagrams(req)) => match req {
                        DaemonUdpOutgoing::Close(close) => {
                            let id = InterceptorId { connection_id: close, protocol: NetProtocol::Datagrams};
                            self.txs.remove(&id);
                        }
                        DaemonUdpOutgoing::Read(read) => self.handle_agent_read(read, NetProtocol::Datagrams).await?,
                        DaemonUdpOutgoing::Connect(connect) => self.handle_connect_response(connect, NetProtocol::Datagrams)?,
                    }
                    Some(OutgoingProxyMessage::LayerConnect(req, message_id, session_id)) => self.handle_connect_request(
                        message_id,
                        session_id,
                        req,
                        message_bus,
                    ).await?,
                    Some(OutgoingProxyMessage::ConnectionRefresh) => self.handle_connection_refresh(),
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
        let outgoing = background_tasks.register(OutgoingProxy::new(Default::default()), (), 8);

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
