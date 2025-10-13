//! Handles the logic of the `outgoing` feature.

use std::{collections::HashMap, fmt, io, ops::Not};

use bytes::Bytes;
use mirrord_intproxy_protocol::{
    LayerId, MessageId, OutgoingConnectRequest, OutgoingConnectResponse, ProxyToLayerMessage,
    SocketMetadataResponse,
};
use mirrord_protocol::{
    DaemonMessage, Payload, ResponseError,
    outgoing::{
        DaemonConnect, LayerWrite, OUTGOING_V2_VERSION, SocketAddress, tcp::DaemonTcpOutgoing,
        udp::DaemonUdpOutgoing, v2,
    },
    uid::Uid,
};
use semver::Version;
use thiserror::Error;
use tracing::Level;

use self::interceptor::Interceptor;
use crate::{
    ProxyMessage,
    background_tasks::{
        BackgroundTask, BackgroundTasks, MessageBus, TaskError, TaskSender, TaskUpdate,
    },
    error::UnexpectedAgentMessage,
    local_sockets::LocalSockets,
    main_tasks::ToLayer,
    proxies::outgoing::net_protocol_ext::{PreparedSocket, prepare_socket},
    request_queue::RequestQueue,
};

mod interceptor;
mod net_protocol_ext;

/// Errors that can occur when handling the `outgoing` feature.
#[derive(Error, Debug)]
pub enum OutgoingProxyError {
    /// The agent sent an error not bound to any [`ConnectionId`].
    ///
    /// This is assumed to be a general agent error.
    /// Originates only from the [`DaemonTcpOutgoing::Connect`]/[`DaemonUdpOutgoing::Connect`]
    /// messages.
    #[error("agent error: {0}")]
    ResponseError(#[from] ResponseError),

    /// The agent sent a [`DaemonConnect`]/[`v2::DaemonOutgoing::Connect`] response,
    /// but the proxy is unable to match it to any known request in progress.
    #[error(transparent)]
    UnexpectedAgentMessage(#[from] Box<UnexpectedAgentMessage>),

    /// The proxy failed to prepare a new local socket for the intercepted connection.
    #[error("failed to prepare a local socket: {0}")]
    SocketSetupError(#[from] io::Error),
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum ConnectionId {
    V1(mirrord_protocol::ConnectionId, v2::OutgoingProtocol),
    V2(mirrord_protocol::uid::Uid),
}

impl fmt::Debug for ConnectionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::V1(id, proto) => write!(f, "{id}-{proto}"),
            Self::V2(id) => id.fmt(f),
        }
    }
}

impl fmt::Display for ConnectionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::V1(id, proto) => write!(f, "{id}-{proto}"),
            Self::V2(id) => id.fmt(f),
        }
    }
}

#[derive(Debug)]
struct ConnectionInProgress {
    message_id: MessageId,
    layer_id: LayerId,
    local_socket: Option<PreparedSocket>,
    remote_address: SocketAddress,
    proto: v2::OutgoingProtocol,
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
    /// In progress v1 remote connect requests for [`v2::OutgoingProtocol::Datagrams`]
    /// ([`LayerUdpOutgoing::Connect`](mirrord_protocol::outgoing::udp::LayerUdpOutgoing::Connect)).
    datagrams_reqs: RequestQueue<ConnectionInProgress>,
    /// In progress v1 remote connect requests for [`v2::OutgoingProtocol::Stream`].
    /// ([`LayerTcpOutgoing::Connect`](mirrord_protocol::outgoing::tcp::LayerTcpOutgoing::Connect)).
    stream_reqs: RequestQueue<ConnectionInProgress>,
    /// In progress [`v2`] remote connect requests.
    reqs: HashMap<Uid, ConnectionInProgress>,

    /// [`TaskSender`]s for active [`Interceptor`] tasks.
    txs: HashMap<ConnectionId, TaskSender<Interceptor>>,
    /// For managing [`Interceptor`] tasks.
    background_tasks: BackgroundTasks<ConnectionId, Bytes, io::Error>,

    /// Stores remote addresses corresponding to local sockets.
    local_sockets: LocalSockets,
    /// [`mirrord_protocol`] version negotiated with the agent.
    protocol_version: Option<Version>,

    /// Whether the local part of TCP connection should be handled in a way that allows for
    /// non-blocking connect from the user application.
    ///
    /// See struct-level docs for more info.
    non_blocking_tcp: bool,
}

impl OutgoingProxy {
    /// Used when registering new [`Interceptor`] tasks in the [`BackgroundTasks`] struct.
    const CHANNEL_SIZE: usize = 512;

    pub fn new(local_sockets: LocalSockets, non_blocking_tcp: bool) -> Self {
        Self {
            datagrams_reqs: Default::default(),
            stream_reqs: Default::default(),
            reqs: Default::default(),
            txs: Default::default(),
            background_tasks: Default::default(),
            local_sockets,
            protocol_version: None,
            non_blocking_tcp,
        }
    }

    /// Retrieves correct v1 [`RequestQueue`] for the given [`v2::OutgoingProtocol`].
    fn queue(&mut self, protocol: v2::OutgoingProtocol) -> &mut RequestQueue<ConnectionInProgress> {
        match protocol {
            v2::OutgoingProtocol::Datagrams => &mut self.datagrams_reqs,
            v2::OutgoingProtocol::Stream => &mut self.stream_reqs,
        }
    }

    /// Passes the data to the correct [`Interceptor`] task.
    async fn handle_data(&self, data: Payload, connection_id: ConnectionId) {
        let data_len = data.len();
        let connection_known = if let Some(interceptor) = self.txs.get(&connection_id) {
            interceptor.send(data.0).await;
            true
        } else {
            false
        };
        tracing::debug!(
            %connection_id,
            data_len,
            connection_known,
            "Received remote data from an outgoing connection.",
        );
    }

    fn handle_close(&mut self, connection_id: ConnectionId) {
        let connection_known = self.txs.remove(&connection_id).is_some();
        tracing::debug!(
            %connection_id,
            connection_known,
            "Received remote close of an outgoing connection.",
        );
    }

    async fn handle_error_v1(
        &mut self,
        protocol: v2::OutgoingProtocol,
        error: ResponseError,
        message_bus: &mut MessageBus<Self>,
    ) -> Result<(), OutgoingProxyError> {
        match self.queue(protocol).pop_front_with_data() {
            Some((message_id, layer_id, in_progress)) => {
                tracing::warn!(
                    %error,
                    remote_address = %in_progress.remote_address,
                    "Received remote error of an outgoing connect attempt.",
                );

                if self.non_blocking_tcp.not() {
                    let to_layer = ToLayer {
                        layer_id,
                        message_id,
                        message: ProxyToLayerMessage::OutgoingConnect(Err(error)),
                    };
                    message_bus.send(to_layer).await;
                }

                Ok(())
            }
            None => {
                let message = protocol.v1_daemon_connect(Err(error));
                Err(Box::new(UnexpectedAgentMessage(message)).into())
            }
        }
    }

    async fn handle_error_v2(
        &mut self,
        error: v2::OutgoingError,
        message_bus: &mut MessageBus<Self>,
    ) {
        let v2::OutgoingError {
            id: connection_id,
            error,
        } = error;
        if let Some(in_progress) = self.reqs.remove(&connection_id) {
            tracing::warn!(
                %connection_id,
                %error,
                remote_address = %in_progress.remote_address,
                "Received remote error of an outgoing connect attempt."
            );

            if self.non_blocking_tcp.not() {
                let to_layer = ToLayer {
                    layer_id: in_progress.layer_id,
                    message_id: in_progress.message_id,
                    message: ProxyToLayerMessage::OutgoingConnect(Err(error)),
                };
                message_bus.send(to_layer).await;
            }
        } else {
            let connection_known = self.txs.remove(&ConnectionId::V2(connection_id)).is_some();
            tracing::warn!(
                %connection_id,
                connection_known,
                %error,
                "Received remote error of an outgoing connection."
            );
        }
    }

    async fn handle_connect_v1(
        &mut self,
        connect: DaemonConnect,
        proto: v2::OutgoingProtocol,
        message_bus: &mut MessageBus<Self>,
    ) -> Result<(), OutgoingProxyError> {
        let (message_id, layer_id, in_progress) = match self.queue(proto).pop_front_with_data() {
            Some(queued) => queued,
            None => {
                let message = proto.v1_daemon_connect(Ok(connect));
                return Err(Box::new(UnexpectedAgentMessage(message)).into());
            }
        };
        let DaemonConnect {
            connection_id,
            local_address: agent_local_address,
            remote_address: agent_peer_address,
        } = connect;
        let connection_id = ConnectionId::V1(connection_id, proto);
        let requested_remote_address = in_progress.remote_address;
        let (local_socket, notify_layer) = match in_progress.local_socket {
            Some(socket) => (socket, false),
            None => {
                let socket =
                    prepare_socket(&requested_remote_address, proto, self.non_blocking_tcp).await?;
                (socket, true)
            }
        };
        let interceptor_address = local_socket.local_address()?;

        tracing::debug!(
            %connection_id,
            %agent_local_address,
            %agent_peer_address,
            %requested_remote_address,
            %interceptor_address,
            "Received remote connect confirmation for an outgoing connection.",
        );
        let entry_guard = self.local_sockets.insert(
            interceptor_address.clone(),
            SocketMetadataResponse {
                agent_address: agent_local_address.clone(),
                peer_address: agent_peer_address.clone(),
            },
        );

        let interceptor = self.background_tasks.register(
            Interceptor::new(connection_id, local_socket, entry_guard),
            connection_id,
            Self::CHANNEL_SIZE,
        );
        self.txs.insert(connection_id, interceptor);

        if notify_layer {
            let to_layer = ToLayer {
                message_id,
                layer_id,
                message: ProxyToLayerMessage::OutgoingConnect(Ok(OutgoingConnectResponse {
                    interceptor_address,
                    agent_local_address: Some(agent_local_address),
                    agent_peer_address: Some(agent_peer_address),
                })),
            };
            message_bus.send(to_layer).await;
        }

        Ok(())
    }

    async fn handle_connect_v2(
        &mut self,
        connect: v2::OutgoingConnectResponse,
        message_bus: &mut MessageBus<Self>,
    ) -> Result<(), OutgoingProxyError> {
        let in_progress = match self.reqs.remove(&connect.id) {
            Some(in_progress) => in_progress,
            None => {
                let message = DaemonMessage::OutgoingV2(v2::DaemonOutgoing::Connect(connect));
                return Err(Box::new(UnexpectedAgentMessage(message)).into());
            }
        };
        let v2::OutgoingConnectResponse {
            id,
            agent_local_address,
            agent_peer_address,
        } = connect;
        let connection_id = ConnectionId::V2(id);
        let requested_remote_address = in_progress.remote_address;
        let (local_socket, notify_layer) = match in_progress.local_socket {
            Some(socket) => (socket, false),
            None => {
                let socket = prepare_socket(
                    &requested_remote_address,
                    in_progress.proto,
                    self.non_blocking_tcp,
                )
                .await?;
                (socket, true)
            }
        };
        let interceptor_address = local_socket.local_address()?;

        tracing::debug!(
            %connection_id,
            %agent_local_address,
            %agent_peer_address,
            %requested_remote_address,
            %interceptor_address,
            "Received remote connect confirmation for an outgoing connection.",
        );
        let entry_guard = self.local_sockets.insert(
            interceptor_address.clone(),
            SocketMetadataResponse {
                agent_address: agent_local_address.clone(),
                peer_address: agent_peer_address.clone(),
            },
        );

        let interceptor = self.background_tasks.register(
            Interceptor::new(connection_id, local_socket, entry_guard),
            connection_id,
            Self::CHANNEL_SIZE,
        );
        self.txs.insert(connection_id, interceptor);

        if notify_layer {
            let to_layer = ToLayer {
                message_id: in_progress.message_id,
                layer_id: in_progress.layer_id,
                message: ProxyToLayerMessage::OutgoingConnect(Ok(OutgoingConnectResponse {
                    interceptor_address,
                    agent_local_address: Some(agent_local_address),
                    agent_peer_address: Some(agent_peer_address),
                })),
            };
            message_bus.send(to_layer).await;
        }

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
        let local_socket = if self.non_blocking_tcp
            && matches!(&request.remote_address, SocketAddress::Ip(..))
            && request.protocol == v2::OutgoingProtocol::Stream
        {
            let local_socket = prepare_socket(
                &request.remote_address,
                request.protocol,
                self.non_blocking_tcp,
            )
            .await?;
            let local_address = local_socket.local_address()?;

            let to_layer = ToLayer {
                layer_id: session_id,
                message_id,
                message: ProxyToLayerMessage::OutgoingConnect(Ok(OutgoingConnectResponse {
                    interceptor_address: local_address,
                    agent_local_address: None,
                    agent_peer_address: None,
                })),
            };
            message_bus.send(to_layer).await;
            Some(local_socket)
        } else {
            None
        };

        let to_agent = if self
            .protocol_version
            .as_ref()
            .is_some_and(|version| OUTGOING_V2_VERSION.matches(version))
        {
            let id = Uid::random();
            self.reqs.insert(
                id,
                ConnectionInProgress {
                    message_id,
                    layer_id: session_id,
                    local_socket,
                    remote_address: request.remote_address.clone(),
                    proto: request.protocol,
                },
            );
            v2::OutgoingConnectRequest {
                id,
                address: request.remote_address,
                protocol: request.protocol,
            }
            .into()
        } else {
            self.queue(request.protocol).push_back_with_data(
                message_id,
                session_id,
                ConnectionInProgress {
                    message_id,
                    layer_id: session_id,
                    local_socket,
                    remote_address: request.remote_address.clone(),
                    proto: request.protocol,
                },
            );
            request.protocol.v1_layer_connect(request.remote_address)
        };
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
        self.reqs = Default::default();
        self.protocol_version = None;
    }
}

/// Messages consumed by the [`OutgoingProxy`] running as a [`BackgroundTask`].
pub enum OutgoingProxyMessage {
    AgentStream(DaemonTcpOutgoing),
    AgentDatagrams(DaemonUdpOutgoing),
    LayerConnect(OutgoingConnectRequest, MessageId, LayerId),
    ConnectionRefresh,
    AgentOutgoing(v2::DaemonOutgoing),
    ProtocolVersion(Version),
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
                            self.handle_close(ConnectionId::V1(close, v2::OutgoingProtocol::Stream));
                        },
                        DaemonTcpOutgoing::Read(Err(error)) => {
                            // This error cannot be tracked to any specific connection.
                            // This is a protocol artifact, we can just exit here.
                            break Err(error.into());
                        },
                        DaemonTcpOutgoing::Read(Ok(data)) => {
                            self.handle_data(data.bytes, ConnectionId::V1(data.connection_id, v2::OutgoingProtocol::Stream)).await;
                        },
                        DaemonTcpOutgoing::Connect(Err(error)) => {
                            self.handle_error_v1(v2::OutgoingProtocol::Stream, error, message_bus).await?;
                        },
                        DaemonTcpOutgoing::Connect(Ok(connect)) => {
                            self.handle_connect_v1(connect, v2::OutgoingProtocol::Stream, message_bus).await?;
                        },
                    },

                    Some(OutgoingProxyMessage::AgentDatagrams(req)) => match req {
                        DaemonUdpOutgoing::Close(close) => {
                            self.handle_close(ConnectionId::V1(close, v2::OutgoingProtocol::Datagrams));
                        },
                        DaemonUdpOutgoing::Read(Err(error)) => {
                            // This error cannot be tracked to any specific connection.
                            // This is a protocol artifact, we can just exit here.
                            break Err(error.into());
                        },
                        DaemonUdpOutgoing::Read(Ok(data)) => {
                            self.handle_data(data.bytes, ConnectionId::V1(data.connection_id, v2::OutgoingProtocol::Datagrams)).await;
                        },
                        DaemonUdpOutgoing::Connect(Err(error)) => {
                            self.handle_error_v1(v2::OutgoingProtocol::Datagrams, error, message_bus).await?;
                        },
                        DaemonUdpOutgoing::Connect(Ok(connect)) => {
                            self.handle_connect_v1(connect, v2::OutgoingProtocol::Datagrams, message_bus).await?;
                        },
                    },

                    Some(OutgoingProxyMessage::AgentOutgoing(msg)) => match msg {
                        v2::DaemonOutgoing::Connect(msg) => {
                            self.handle_connect_v2(msg, message_bus).await?;
                        }
                        v2::DaemonOutgoing::Data(msg) => {
                            self.handle_data(msg.data, ConnectionId::V2(msg.id)).await;
                        }
                        v2::DaemonOutgoing::Close(msg) => {
                            self.handle_close(ConnectionId::V2(msg.id));
                        }
                        v2::DaemonOutgoing::Error(msg) => {
                            self.handle_error_v2(msg, message_bus).await;
                        }
                    },

                    Some(OutgoingProxyMessage::LayerConnect(req, message_id, session_id)) => self.handle_connect_request(
                        message_id,
                        session_id,
                        req,
                        message_bus,
                    ).await?,

                    Some(OutgoingProxyMessage::ProtocolVersion(version)) => {
                        self.protocol_version.replace(version);
                    }

                    Some(OutgoingProxyMessage::ConnectionRefresh) => self.handle_connection_refresh(),
                },

                Some(task_update) = self.background_tasks.next() => match task_update {
                    (id, TaskUpdate::Message(bytes)) => {
                        let msg = match id {
                            ConnectionId::V1(id, proto) => {
                                proto.v1_layer_write(LayerWrite { connection_id: id, bytes: bytes.into() })
                            }
                            ConnectionId::V2(id) => {
                                v2::OutgoingData {
                                    id,
                                    data: bytes.into(),
                                }.into()
                            }
                        };
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
                            let msg = match id {
                                ConnectionId::V1(id, proto) => {
                                    proto.v1_layer_close(id)
                                }
                                ConnectionId::V2(id) => {
                                    v2::OutgoingClose {
                                        id,
                                    }.into()
                                }
                            };
                            message_bus.send(ProxyMessage::ToAgent(msg)).await;
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

    use mirrord_intproxy_protocol::{LayerId, OutgoingConnectRequest, ProxyToLayerMessage};
    use mirrord_protocol::{
        ClientMessage,
        outgoing::{
            DaemonConnect, LayerConnect, SocketAddress,
            tcp::{DaemonTcpOutgoing, LayerTcpOutgoing},
            v2,
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
        let outgoing =
            background_tasks.register(OutgoingProxy::new(Default::default(), true), (), 8);

        for i in 0..=1 {
            // Layer wants to make an outgoing connection.
            outgoing
                .send(OutgoingProxyMessage::LayerConnect(
                    OutgoingConnectRequest {
                        remote_address: SocketAddress::Ip(peer_addr),
                        protocol: v2::OutgoingProtocol::Stream,
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
