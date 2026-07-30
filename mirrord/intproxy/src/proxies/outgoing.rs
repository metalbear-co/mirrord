//! Handles the logic of the `outgoing` feature.

use std::{
    collections::{HashMap, HashSet},
    fmt, io,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    ops::{ControlFlow, Not},
    sync::Arc,
    time::{Duration, Instant},
};

use bytes::Bytes;
use mirrord_intproxy_protocol::{
    LayerId, MessageId, NetProtocol, OutgoingConnMetadataResponse, OutgoingConnectRequest,
    OutgoingConnectResponse, OutgoingRequest, OutgoingResponse, ProxyToLayerMessage,
};
#[cfg(target_os = "linux")]
use mirrord_protocol::outgoing::OUTGOING_SEQPACKET;
use mirrord_protocol::{
    ConnectionId, DaemonMessage, RemoteResult, ResponseError,
    outgoing::{
        DaemonConnect, DaemonConnectV2, DaemonRead, OUTGOING_CONNECT_V2, SocketAddress,
        seqpacket::DaemonSeqpacket, tcp::DaemonTcpOutgoing, udp::DaemonUdpOutgoing,
    },
    uid::Uid,
};
use semver::Version;
use thiserror::Error;
use tokio::{
    net::TcpListener,
    sync::{OwnedSemaphorePermit, Semaphore},
};
use tracing::Level;

use self::interceptor::{
    Interceptor, InterceptorCommand, WRITE_BUDGET_BYTES, read_queue::InterceptorReadQueue,
    write_queue::AgentWriteQueue,
};
use crate::{
    ProxyMessage,
    background_tasks::{BackgroundTask, BackgroundTasks, MessageBus, TaskError, TaskUpdate},
    error::{UnexpectedAgentMessage, agent_lost_io_error},
    main_tasks::{ConnectionRefresh, DnsFilteringLookupResult, LayerClosed, LayerForked, ToLayer},
    proxies::outgoing::{
        dns::{DnsFiltering, DnsTunnelRequest},
        net_protocol_ext::{NetProtocolExt, PreparedSocket},
    },
    remote_resources::RemoteResources,
    request_queue::RequestQueue,
    session_monitor::chaos::{ChaosWatcherRx, rules::ConnectionErrorType},
};

mod chaos;
pub mod dns;
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
    #[error("failed to prepare local socket: {0}")]
    SocketSetupError(#[from] io::Error),

    /// The agent sent a queue-based seqpacket connect response. Seqpacket supports only ConnectV2.
    ///
    /// Should never really happen, it should not be possible to start a mirrord managed
    /// `SOCK_SEQPACKET` socket with a mix of mirrord-cli and mirrord-agent versions that do not
    /// support `ConnectV2`.
    #[error("agent sent an unexpected seqpacket Connect response")]
    UnexpectedSeqpacketConnect,
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
            message: ProxyToLayerMessage::Outgoing(OutgoingResponse::Connect(Err(error))),
        }
    }
}

#[derive(Debug)]
struct ConnectInProgress {
    prepared_listener: Option<TcpListener>,
    remote_address: SocketAddress,
    hostname: Option<String>,
    requested_at: Instant,
    layer_id: LayerId,
    message_id: MessageId,
    id: u128,
    /// Set when this connection is being made on behalf of an already intercepted DNS socket
    /// that fell back to tunneling, in which case the layer has long been answered and the
    /// socket already exists. See [`dns`].
    dns_socket: Option<InterceptorId>,
}

pub struct DeferredConnection {
    request: OutgoingConnectRequest,
    message_id: MessageId,
    layer_id: LayerId,
}

/// Original connection metadata requested by the layer.
///
/// Say the user app wants to make a connection to `https://www.przepisy.pl/`,
/// the `remote_address` has the ip + port, and the `hostname` is `www.przepisy.pl`.
#[derive(Debug)]
struct InterceptorConnectionInfo {
    /// Ip or Unix address of this connection (as originally requested).
    remote_address: SocketAddress,
    /// Hostname of this connection, if any (as originally requested).
    hostname: Option<String>,
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
/// Experimental and disabled by default.
///
/// 1. Proxy receives an [`OutgoingConnectRequest`] from the layer.
/// 2. Proxy sends a corresponding [`LayerConnect`](mirrord_protocol::outgoing::LayerConnect) to the
///    agent.
/// 3. Proxy creates a new [`TcpListener`], and sends confirmation to the layer.
/// 4. The layer connects to the socket immediately.
/// 5. Proxy receives a confirmation from the agent.
/// 6. Proxy starts a new outgoing [`Interceptor`] background task to manage the connection.
/// 7. The proxy passes the data between the agent and the [`Interceptor`] task.
/// 8. If the layer closes the connection, the [`Interceptor`] exits and the proxy notifies the
///    agent. If the agent closes the connection, the proxy shuts down the [`Interceptor`].
///
/// The downside here is that the proxying of outgoing connections
/// stops being "transparent" to the app, i.e. the existence of a TCP
/// proxy becomes observable to the app. (e.g. if the agent <-> target
/// connection fails, this will appear to the app as the connection
/// getting accepted and subsequently dropped, when it was never
/// accepted in the first place). Thankfully this should not affect
/// the operation of any app unless it's doing something *very*
/// nonstandard, in which case they can just use the blocking flow.
///
/// ## Why?
///
/// In the regular flow, the user app's thread is unconditionally **blocked** during intproxy's
/// exchange with the agent (this includes making the actual remote connection on the agent
/// side). If the user app's socket is blocking, this is perfectly fine.
///
/// However, if the user app's socket is non-blocking, this is not what is expected.
/// The user app expects the `connect` call to return instantly with `EINPROGRESS`,
/// so that the socket can later be polled for write readiness, and the thread can do other work.
/// The most extreme case here is NodeJS, which is single threaded by design.
///
/// Consider the following scenario:
/// 1. mirrord steals an HTTP request for the user app (NodeJS) to handle.
/// 2. To handle the request, the app makes multiple HTTP requests to downstream services.
/// 3. For each request, a new outgoing connection is made.
/// 4. If the [`OutgoingProxy`] does not use the non-blocking flow, each outgoing connect attempt
///    will effectively *freeze* the NodeJS reactor for some time (observed in real life to be over
///    200ms). Latency goes through the roof. Also, it affects all other async tasks/promises.
pub struct OutgoingProxy {
    /// In progress [`OutgoingConnectRequest`]s originating from
    /// [`LayerConnect`](mirrord_protocol::outgoing::LayerConnect), related to
    /// [`NetProtocol::Datagrams`].
    ///
    /// These are processed sequentially by the agent.
    datagrams_reqs: RequestQueue<ConnectInProgress>,
    /// In progress [`OutgoingConnectRequest`]s originating from
    /// [`LayerConnect`](mirrord_protocol::outgoing::LayerConnect), related to
    /// [`NetProtocol::Stream`].
    ///
    /// These are processed sequentially by the agent.
    stream_reqs: RequestQueue<ConnectInProgress>,
    /// In progress [`OutgoingConnectRequest`]s originating from
    /// [`LayerConnectV2`](mirrord_protocol::outgoing::LayerConnectV2).
    ///
    /// These are processed in parallel by the agent.
    v2_reqs: HashMap<(Uid, NetProtocol), ConnectInProgress>,

    /// Per-interceptor FIFO queues for messages sent to the layer.
    interceptor_read_queues: HashMap<InterceptorId, InterceptorReadQueue>,
    /// For managing [`Interceptor`] tasks.
    background_tasks:
        Option<BackgroundTasks<InterceptorId, (Bytes, OwnedSemaphorePermit), io::Error>>,
    /// Per-interceptor FIFO queues for messages sent to the agent.
    agent_write_queues: HashMap<InterceptorId, AgentWriteQueue>,

    /// Whether TCP connect requests should be handled in a non-blocking way.
    ///
    /// See struct level docs for more info.
    non_blocking_tcp_connect: bool,
    /// DNS traffic that this proxy answers itself instead of tunneling. See [`dns`].
    dns: DnsFiltering,
    /// Established version of the [`mirrord_protocol`].
    protocol_version: Option<Version>,

    /// Delay to apply to receive operations (Agent → Layer), in milliseconds.
    receive_delay_ms: u64,

    /// Delay to apply to transmit operations (Layer → Agent), in milliseconds.
    transmit_delay_ms: u64,

    /// Outgoing connection local IDs, by layer instance.
    ///
    /// Local IDs are random and generated in this proxy.
    /// We can't use [`ConnectionId`] returned from the agent,
    /// because we need some ID as soon as we receive the connect request from the layer.
    connections_in_layers: RemoteResources<u128>,
    /// Maps outgoing connection local IDs to local addresses of corresponding agent sockets.
    agent_local_addresses: HashMap<u128, SocketAddr>,
    /// Original connection metadata requested by the layer, keyed by active interceptor id.
    interceptor_connection_info: HashMap<InterceptorId, InterceptorConnectionInfo>,
    /// Connections that should no longer exchange bytes with the layer due to chaos.
    ///
    /// Translating: we have some chaos connection errors that can happen for ongoing connections,
    /// and there's no way to send back to the layer something like "Hey bro, timeout the
    /// connection on this socket.", so we block the interceptor, meaning the layer keeps sending
    /// `write`s, thinking everything is fine, until the user's app reaches a timeout on its own.
    chaos_blocked_interceptors: HashSet<InterceptorId>,

    /// State where we hold all the `ChaosRule`s for this intproxy.
    chaos_rx: ChaosWatcherRx,
}

impl OutgoingProxy {
    /// Used when registering new [`Interceptor`] tasks in the [`BackgroundTasks`] struct.
    pub(crate) const CHANNEL_SIZE: usize = 512;

    /// Gets the [`InterceptorConnectionInfo`] for this connection with `interceptor_id`.
    ///
    /// We can use this to get connection information about ongoing connections, after they have
    /// been established (i.e. to get this information when we received a write message for this
    /// connection).
    fn connection_info(&self, interceptor_id: InterceptorId) -> Option<&InterceptorConnectionInfo> {
        self.interceptor_connection_info.get(&interceptor_id)
    }

    fn supports_connect_v2(&self) -> bool {
        self.protocol_version
            .as_ref()
            .is_some_and(|version| OUTGOING_CONNECT_V2.matches(version))
    }

    /// Creates a new instance, ready to run.
    ///
    /// # Params
    ///
    /// * `non_blocking_tcp_connect` - see struct level docs
    /// * `dns_filtering` - whether to answer DNS traffic locally, see [`dns`]
    /// * `receive_delay_ms` - delay in milliseconds for receive operations (Agent → Layer)
    /// * `transmit_delay_ms` - delay in milliseconds for transmit operations (Layer → Agent)
    pub fn new(
        non_blocking_tcp_connect: bool,
        dns_filtering: bool,
        receive_delay_ms: u64,
        transmit_delay_ms: u64,
        chaos_rx: ChaosWatcherRx,
    ) -> Self {
        Self {
            datagrams_reqs: Default::default(),
            stream_reqs: Default::default(),
            v2_reqs: Default::default(),
            interceptor_read_queues: Default::default(),
            background_tasks: Default::default(),
            agent_write_queues: Default::default(),
            non_blocking_tcp_connect,
            dns: DnsFiltering::new(dns_filtering),
            protocol_version: Default::default(),
            receive_delay_ms,
            transmit_delay_ms,
            connections_in_layers: Default::default(),
            agent_local_addresses: Default::default(),
            interceptor_connection_info: Default::default(),
            chaos_blocked_interceptors: Default::default(),
            chaos_rx,
        }
    }

    /// Retrieves correct [`RequestQueue`] for the given [`NetProtocol`].
    fn queue(&mut self, protocol: NetProtocol) -> &mut RequestQueue<ConnectInProgress> {
        match protocol {
            NetProtocol::Datagrams => &mut self.datagrams_reqs,
            NetProtocol::Stream => &mut self.stream_reqs,
            NetProtocol::Seqpacket => unreachable!(
                "BUG: should not be possible to create a `SOCK_SEQPACKET` socket connection using this outdated queue!"
            ),
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

        if self.dns.owns_connection(connection_id, protocol) {
            self.dns
                .handle_agent_read(connection_id, protocol, bytes.0)
                .await;
            return Ok(());
        }

        let id = InterceptorId {
            connection_id,
            protocol,
        };

        if self.chaos_blocked_interceptors.contains(&id) {
            return Ok(());
        }

        let delay = self
            .chaos_read_latency_for_connection(id)
            .unwrap_or_else(|| Duration::from_millis(self.receive_delay_ms));
        if self
            .queue_interceptor_command(id, InterceptorCommand::Data(bytes.0), delay)
            .await
            .not()
        {
            tracing::trace!(
                "{id} does not exist, received data for connection that is already closed"
            );
        }

        Ok(())
    }

    /// Tears down the local side of a connection that the agent closed.
    #[tracing::instrument(level = Level::TRACE, skip(self))]
    async fn handle_agent_close(&mut self, connection_id: ConnectionId, protocol: NetProtocol) {
        if self.dns.handle_agent_close(connection_id, protocol).await {
            return;
        }

        let id = InterceptorId {
            connection_id,
            protocol,
        };

        self.abort_agent_write_queue(&id);

        if self.chaos_blocked_interceptors.contains(&id).not() {
            self.interceptor_connection_info.remove(&id);
            self.finish_interceptor_read_queue(id, InterceptorCommand::Shutdown, Duration::ZERO)
                .await;
        }
    }

    /// Handles agent's response to a connection request.
    /// Prepares a local socket and registers a new [`Interceptor`] task for this connection.
    /// Replies to the layer's request.
    #[tracing::instrument(level = Level::DEBUG, skip(self, message_bus), ret, err)]
    async fn handle_connect_response(
        &mut self,
        connect: RemoteResult<DaemonConnect>,
        protocol: NetProtocol,
        uid: Option<Uid>,
        message_bus: &mut MessageBus<Self>,
    ) -> Result<(), OutgoingProxyError> {
        let in_progress = match uid {
            Some(uid) => self.v2_reqs.remove(&(uid, protocol)),
            None => self
                .queue(protocol)
                .pop_front_with_data()
                .map(|(_, _, in_progress)| in_progress),
        };
        let Some(in_progress) = in_progress else {
            let message =
                match (uid, protocol) {
                    (Some(uid), NetProtocol::Datagrams) => {
                        DaemonMessage::UdpOutgoing(DaemonUdpOutgoing::ConnectV2(DaemonConnectV2 {
                            uid,
                            connect,
                        }))
                    }
                    (None, NetProtocol::Datagrams) => {
                        DaemonMessage::UdpOutgoing(DaemonUdpOutgoing::Connect(connect))
                    }
                    (Some(uid), NetProtocol::Stream) => {
                        DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::ConnectV2(DaemonConnectV2 {
                            uid,
                            connect,
                        }))
                    }
                    (None, NetProtocol::Stream) => {
                        DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Connect(connect))
                    }
                    (Some(uid), NetProtocol::Seqpacket) => DaemonMessage::SeqpacketOutgoing(
                        DaemonSeqpacket::ConnectV2(DaemonConnectV2 { uid, connect }),
                    ),
                    (None, NetProtocol::Seqpacket) => {
                        return Err(OutgoingProxyError::UnexpectedSeqpacketConnect);
                    }
                };
            return Err(UnexpectedAgentMessage(message.into()).into());
        };

        if let Some(dns_socket) = in_progress.dns_socket {
            self.dns
                .handle_tunnel_connected(dns_socket, connect, message_bus)
                .await;
            return Ok(());
        }

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

                if in_progress.prepared_listener.is_none() {
                    message_bus
                        .send(ToLayer {
                            message: ProxyToLayerMessage::Outgoing(OutgoingResponse::Connect(Err(
                                error,
                            ))),
                            message_id: in_progress.message_id,
                            layer_id: in_progress.layer_id,
                        })
                        .await;
                }

                return Ok(());
            }
        };

        if let SocketAddress::Ip(addr) = &local_address {
            self.agent_local_addresses.insert(in_progress.id, *addr);
        }

        let prepared_socket = match in_progress.prepared_listener {
            Some(listener) => PreparedSocket::TcpListener(listener),
            None => {
                let prepared_socket = protocol.prepare_socket(remote_address).await?;
                let layer_address = prepared_socket.local_address()?;

                message_bus
                    .send(ToLayer {
                        message: ProxyToLayerMessage::Outgoing(OutgoingResponse::Connect(Ok(
                            OutgoingConnectResponse {
                                connection_id: in_progress.id,
                                layer_address,
                                in_cluster_address: Some(local_address),
                            },
                        ))),
                        message_id: in_progress.message_id,
                        layer_id: in_progress.layer_id,
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
        let write_budget = Arc::new(Semaphore::new(WRITE_BUDGET_BYTES));
        let interceptor = self.background_tasks.as_mut().unwrap().register(
            Interceptor::new(id, prepared_socket, write_budget),
            id,
            Self::CHANNEL_SIZE,
        );
        let agent_write_queue = AgentWriteQueue::new(message_bus.clone_agent_tx());
        let interceptor_read_queue = InterceptorReadQueue::new(interceptor);
        self.interceptor_connection_info.insert(
            id,
            InterceptorConnectionInfo {
                remote_address: in_progress.remote_address,
                hostname: in_progress.hostname,
            },
        );
        self.agent_write_queues.insert(id, agent_write_queue);
        self.interceptor_read_queues
            .insert(id, interceptor_read_queue);

        Ok(())
    }

    /// Saves the layer's request id and sends the connection request to the agent.
    async fn handle_connect_request(
        &mut self,
        message_id: MessageId,
        session_id: LayerId,
        request: OutgoingConnectRequest,
        message_bus: &mut MessageBus<Self>,
    ) -> Result<(), OutgoingProxyError> {
        #[cfg(target_os = "linux")]
        let supports_seqpacket = self
            .protocol_version
            .as_ref()
            .is_some_and(|version| OUTGOING_SEQPACKET.matches(version));

        #[cfg(not(target_os = "linux"))]
        let supports_seqpacket = false;

        if (request.protocol == NetProtocol::Seqpacket) && supports_seqpacket.not() {
            message_bus
                .send(ToLayer {
                    message: ProxyToLayerMessage::Outgoing(OutgoingResponse::Connect(Err(
                        ResponseError::NotImplemented,
                    ))),
                    message_id,
                    layer_id: session_id,
                })
                .await;
            return Ok(());
        }

        // The chance for collision here is negligible.
        let connection_id = rand::random::<u128>();
        self.connections_in_layers.add(session_id, connection_id);

        if self.dns.should_intercept(&request) {
            return self
                .dns
                .intercept_connection(connection_id, message_id, session_id, request, message_bus)
                .await
                .map_err(Into::into);
        }

        let prepared_stream = if self.non_blocking_tcp_connect
            && request.protocol == NetProtocol::Stream
            && let SocketAddress::Ip(ip) = request.remote_address
        {
            let bind_addr = if ip.is_ipv4() {
                SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 0)
            } else {
                SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), 0)
            };

            let listener = TcpListener::bind(bind_addr).await?;
            let addr = listener.local_addr()?;
            let to_layer = ToLayer {
                message_id,
                layer_id: session_id,
                message: ProxyToLayerMessage::Outgoing(OutgoingResponse::Connect(Ok(
                    OutgoingConnectResponse {
                        connection_id,
                        layer_address: addr.into(),
                        in_cluster_address: None,
                    },
                ))),
            };

            message_bus.send(to_layer).await;
            Some(listener)
        } else {
            None
        };

        let in_progress = ConnectInProgress {
            id: connection_id,
            prepared_listener: prepared_stream,
            remote_address: request.remote_address.clone(),
            hostname: request.hostname().cloned(),
            requested_at: Instant::now(),
            layer_id: session_id,
            message_id,
            dns_socket: None,
        };

        self.request_connection(
            request.protocol,
            request.remote_address,
            in_progress,
            message_bus,
        )
        .await;

        Ok(())
    }

    /// Remembers a pending connect request and asks the agent to make the connection.
    async fn request_connection(
        &mut self,
        protocol: NetProtocol,
        remote_address: SocketAddress,
        in_progress: ConnectInProgress,
        message_bus: &mut MessageBus<Self>,
    ) {
        let uid = if self.supports_connect_v2() {
            let request_uid = Uid::new_v4();
            self.v2_reqs.insert((request_uid, protocol), in_progress);
            Some(request_uid)
        } else {
            let ConnectInProgress {
                message_id,
                layer_id,
                ..
            } = in_progress;
            self.queue(protocol)
                .push_back_with_data(message_id, layer_id, in_progress);
            None
        };

        message_bus
            .send_agent(protocol.wrap_agent_connect(remote_address, uid))
            .await;
    }

    /// Opens the connection an intercepted DNS socket asked for after hitting a query that we
    /// cannot answer ourselves. See [`dns`].
    async fn request_dns_tunnel(
        &mut self,
        request: DnsTunnelRequest,
        message_bus: &mut MessageBus<Self>,
    ) {
        let DnsTunnelRequest {
            id,
            layer_id,
            remote_address,
        } = request;

        let in_progress = ConnectInProgress {
            // The layer was answered when the socket was intercepted, so there is no request of
            // its own to reply to, and no local connection id to track.
            id: 0,
            message_id: 0,
            prepared_listener: None,
            remote_address: remote_address.clone(),
            hostname: None,
            requested_at: Instant::now(),
            layer_id,
            dns_socket: Some(id),
        };

        self.request_connection(id.protocol, remote_address, in_progress, message_bus)
            .await;
    }

    /// Tells the layer that a pending connect request died with the agent connection.
    ///
    /// Requests made for an intercepted DNS socket have no layer request behind them, so there
    /// is nobody to answer; the socket itself was already dropped by [`DnsFiltering::clear`].
    async fn flush_lost_connect_request(
        in_progress: ConnectInProgress,
        message_id: MessageId,
        layer_id: LayerId,
        message_bus: &mut MessageBus<Self>,
    ) {
        if in_progress.dns_socket.is_some() {
            return;
        }

        message_bus
            .send(ToLayer::from(AgentLostOutgoingResponse(
                layer_id, message_id,
            )))
            .await;
    }

    #[tracing::instrument(level = Level::INFO, skip_all, ret)]
    async fn handle_connection_refresh(
        &mut self,
        message_bus: &mut MessageBus<Self>,
        refresh: ConnectionRefresh,
    ) {
        match refresh {
            ConnectionRefresh::Start => {
                tracing::debug!("Closing all local connections");
                self.interceptor_connection_info.clear();
                self.chaos_blocked_interceptors.clear();
                self.background_tasks.as_mut().unwrap().clear();
                self.dns.clear();
                self.abort_all_agent_write_queues();
                self.abort_all_interceptor_read_queues();
                self.protocol_version = None;

                tracing::debug!(
                    responses = self.datagrams_reqs.len(),
                    "Flushing error responses to UDP connect requests"
                );
                while let Some((message_id, layer_id, in_progress)) =
                    self.datagrams_reqs.pop_front_with_data()
                {
                    Self::flush_lost_connect_request(
                        in_progress,
                        message_id,
                        layer_id,
                        message_bus,
                    )
                    .await;
                }

                tracing::debug!(
                    responses = self.stream_reqs.len(),
                    "Flushing error responses to TCP connect requests"
                );
                while let Some((message_id, layer_id, in_progress)) =
                    self.stream_reqs.pop_front_with_data()
                {
                    Self::flush_lost_connect_request(
                        in_progress,
                        message_id,
                        layer_id,
                        message_bus,
                    )
                    .await;
                }

                tracing::debug!(
                    responses = self.v2_reqs.len(),
                    "Flushing error responses to V2 connect requests"
                );
                for in_progress in std::mem::take(&mut self.v2_reqs).into_values() {
                    let (message_id, layer_id) = (in_progress.message_id, in_progress.layer_id);
                    Self::flush_lost_connect_request(
                        in_progress,
                        message_id,
                        layer_id,
                        message_bus,
                    )
                    .await;
                }

                // Reset protocol version since we'll need another negotiation
                // round for the new connection.
                self.protocol_version = None;
            }
            ConnectionRefresh::End(tx_handle) => {
                message_bus.set_agent_tx(tx_handle);
            }
            ConnectionRefresh::Request => {}
        }
    }

    #[tracing::instrument(level = Level::DEBUG, skip(self, message_bus), ret, err(level = Level::DEBUG))]
    async fn handle_layer_request(
        &mut self,
        request: OutgoingRequest,
        layer_id: LayerId,
        message_id: MessageId,
        message_bus: &mut MessageBus<Self>,
    ) -> Result<(), OutgoingProxyError> {
        match request {
            OutgoingRequest::Connect(req) => {
                self.handle_connect_request(message_id, layer_id, req, message_bus)
                    .await
            }
            OutgoingRequest::ConnMetadata(req) => {
                let response =
                    self.agent_local_addresses.get(&req.conn_id).copied().map(
                        |in_cluster_address| OutgoingConnMetadataResponse { in_cluster_address },
                    );
                let to_layer = ToLayer {
                    message_id,
                    layer_id,
                    message: ProxyToLayerMessage::Outgoing(OutgoingResponse::ConnMetadata(
                        response,
                    )),
                };
                message_bus.send(to_layer).await;
                Ok(())
            }
            OutgoingRequest::Close(req) => {
                if self.connections_in_layers.remove(layer_id, req.conn_id) {
                    self.agent_local_addresses.remove(&req.conn_id);
                }
                Ok(())
            }
        }
    }
}

/// Messages consumed by the [`OutgoingProxy`] running as a [`BackgroundTask`].
pub enum OutgoingProxyMessage {
    AgentStream(DaemonTcpOutgoing),
    AgentDatagrams(DaemonUdpOutgoing),
    AgentSeqpacket(DaemonSeqpacket),
    AgentProtocolVersion(Version),
    Layer(OutgoingRequest, MessageId, LayerId),
    DeferredConnect(DeferredConnection),
    ConnectionRefresh(ConnectionRefresh),
    LayerForked(LayerForked),
    LayerClosed(LayerClosed),
    /// Remote resolution of a DNS query intercepted by [`DnsFiltering`] finished.
    DnsFilteringLookupResult(DnsFilteringLookupResult),
}

impl BackgroundTask for OutgoingProxy {
    type Error = OutgoingProxyError;
    type MessageIn = OutgoingProxyMessage;
    type MessageOut = ProxyMessage;

    #[tracing::instrument(level = Level::INFO, name = "outgoing_proxy_main_loop", skip_all, ret, err)]
    async fn run(&mut self, message_bus: &mut MessageBus<Self>) -> Result<(), Self::Error> {
        match &mut self.background_tasks {
            Some(tasks) => tasks.set_agent_tx(message_bus.clone_agent_tx()),
            None => {
                self.background_tasks = Some(BackgroundTasks::new(message_bus.clone_agent_tx()))
            }
        };
        self.dns.attach(message_bus);

        loop {
            tokio::select! {
                msg = message_bus.recv() => match msg {
                    None => {
                        tracing::debug!("Message bus closed, exiting");
                        break Ok(());
                    },
                    Some(OutgoingProxyMessage::AgentStream(req)) => match req {
                        DaemonTcpOutgoing::Close(close) => {
                            self.handle_agent_close(close, NetProtocol::Stream).await;
                        },
                        DaemonTcpOutgoing::Read(read) => self.handle_agent_read(read, NetProtocol::Stream).await?,
                        DaemonTcpOutgoing::Connect(connect) => self.handle_connect_response(connect, NetProtocol::Stream, None, message_bus).await?,
                        DaemonTcpOutgoing::ConnectV2(connect) => self.handle_connect_response(
                            connect.connect,
                            NetProtocol::Stream,
                            Some(connect.uid),
                            message_bus,
                        ).await?,
                    }
                    Some(OutgoingProxyMessage::AgentDatagrams(req)) => match req {
                        DaemonUdpOutgoing::Close(close) => {
                            self.handle_agent_close(close, NetProtocol::Datagrams).await;
                        }
                        DaemonUdpOutgoing::Read(read) => self.handle_agent_read(read, NetProtocol::Datagrams).await?,
                        DaemonUdpOutgoing::Connect(connect) => self.handle_connect_response(connect, NetProtocol::Datagrams, None, message_bus).await?,
                        DaemonUdpOutgoing::ConnectV2(connect) => self.handle_connect_response(
                            connect.connect,
                            NetProtocol::Datagrams,
                            Some(connect.uid),
                            message_bus,
                        ).await?,
                    }
                    Some(OutgoingProxyMessage::AgentSeqpacket(req)) => match req {
                        DaemonSeqpacket::Close(close) => {
                            self.handle_agent_close(close, NetProtocol::Seqpacket).await;
                        }
                        DaemonSeqpacket::Read(read) => self.handle_agent_read(read, NetProtocol::Seqpacket).await?,
                        DaemonSeqpacket::ConnectV2(connect) => self.handle_connect_response(
                            connect.connect,
                            NetProtocol::Seqpacket,
                            Some(connect.uid),
                            message_bus,
                        ).await?,
                    }
                    Some(OutgoingProxyMessage::Layer(request, message_id, layer_id)) => match request {
                        OutgoingRequest::Connect(connect) => {
                            if self.chaos_effect_for_connect_error(&connect, message_id, layer_id, message_bus).await.is_break() {
                                continue;
                            }

                            let connect = if self.supports_connect_v2() {
                                match self.chaos_effect_for_connect_latency(connect, message_id, layer_id, message_bus).await {
                                    ControlFlow::Continue(connect) => connect,
                                    ControlFlow::Break(()) => continue,
                                }
                            } else {
                                connect
                            };

                            self.handle_connect_request(message_id, layer_id, connect, message_bus).await?;
                        }
                        request => {
                            self.handle_layer_request(request, layer_id, message_id, message_bus).await?;
                        }
                    },
                    Some(OutgoingProxyMessage::DeferredConnect(DeferredConnection {
                        request,
                        message_id,
                        layer_id,
                    })) => {
                        self.handle_connect_request(message_id, layer_id, request, message_bus).await?;
                    }
                    Some(OutgoingProxyMessage::LayerForked(forked)) => {
                        self.connections_in_layers.clone_all(forked.parent, forked.child);
                    }
                    Some(OutgoingProxyMessage::LayerClosed(closed)) => {
                        for id in self.connections_in_layers.remove_all(closed.id) {
                            self.agent_local_addresses.remove(&id);
                        }
                        self.dns.layer_closed(closed.id);
                    }
                    Some(OutgoingProxyMessage::DnsFilteringLookupResult(result)) => {
                        self.dns.handle_lookup_result(result).await;
                    }
                    Some(OutgoingProxyMessage::ConnectionRefresh(refresh)) => self.handle_connection_refresh(message_bus, refresh).await,
                    Some(OutgoingProxyMessage::AgentProtocolVersion(version)) => {
                        self.protocol_version.replace(version);
                    }
                },

                Some(task_update) = self.background_tasks.as_mut().unwrap().next() => match task_update {
                    (id, TaskUpdate::Message((bytes, permit))) => {
                        if self.chaos_blocked_interceptors.contains(&id) {
                            continue;
                        }

                        if let Some(effect) = self.chaos_connection_error_for_ongoing_connection(id) {
                            let delay = effect.after;
                            self.chaos_blocked_interceptors.insert(id);

                            match effect.error_type {
                                ConnectionErrorType::Reset => {
                                    self.interceptor_connection_info.remove(&id);

                                    let close_msg = id.protocol.wrap_agent_close(id.connection_id);
                                    self.finish_agent_write_queue(id, close_msg, delay).await;
                                    self.finish_interceptor_read_queue(id, InterceptorCommand::Reset, delay).await;
                                }
                                ConnectionErrorType::TimedOut => {
                                    self.interceptor_connection_info.remove(&id);
                                    self.abort_agent_write_queue(&id);
                                    self.queue_interceptor_command(id, InterceptorCommand::Stall, delay).await;
                                }
                                ConnectionErrorType::Refused => {
                                    unreachable!("BUG: we should never get a Refused or Unknown here, \
                                        please report it to us!")
                                }
                            }
                            continue;
                        }

                        let delay = self
                            .chaos_write_latency_for_connection(id)
                            .unwrap_or_else(|| Duration::from_millis(self.transmit_delay_ms));
                        let msg = id.protocol.wrap_agent_write(id.connection_id, bytes);
                        self.queue_agent_message(id, msg, delay, Some(permit)).await;
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

                        let was_chaos_blocked = self.chaos_blocked_interceptors.remove(&id);

                        if self.abort_interceptor_read_queue(&id) {
                            self.interceptor_connection_info.remove(&id);
                            tracing::trace!(%id, "Local connection closed, notifying the agent");
                            let msg = id.protocol.wrap_agent_close(id.connection_id);
                            self.finish_agent_write_queue(id, msg, Duration::ZERO).await;
                        } else if was_chaos_blocked {
                            self.interceptor_connection_info.remove(&id);
                        }
                    }
                },

                Some(task_update) = self.dns.next_update() => match task_update {
                    // The write budget is irrelevant here: DNS messages are tiny, and either
                    // answered from this proxy or forwarded to the agent immediately.
                    (id, TaskUpdate::Message((bytes, _permit))) => {
                        if let Some(request) = self.dns.handle_read(id, bytes, message_bus).await {
                            self.request_dns_tunnel(request, message_bus).await;
                        }
                    }
                    (id, TaskUpdate::Finished(res)) => {
                        self.dns.handle_finished(id, res, message_bus).await;
                    }
                },
            }
        }
    }
}

#[cfg(test)]
mod test {
    use std::{
        net::{Ipv4Addr, SocketAddr},
        time::Duration,
    };

    use hickory_proto::{
        op::{Message, Query},
        rr::{Name, RData, RecordType, rdata::A},
    };
    use mirrord_intproxy_protocol::{
        LayerId, NetProtocol, OutgoingConnectRequest, OutgoingConnectRequestMetadata,
        OutgoingRequest, OutgoingResponse, ProxyToLayerMessage,
    };
    use mirrord_protocol::{
        ClientMessage,
        dns::{DnsLookup, LookupRecord},
        outgoing::{
            DaemonConnect, LayerConnect, SocketAddress,
            tcp::{DaemonTcpOutgoing, LayerTcpOutgoing},
            udp::LayerUdpOutgoing,
        },
    };
    use mirrord_protocol_io::{Client, Connection, ConnectionOutput};
    use tokio::{net::UdpSocket, sync::watch, time::timeout};

    use crate::{
        background_tasks::{BackgroundTasks, TaskSender, TaskUpdate},
        main_tasks::{ConnectionRefresh, DnsFilteringLookupResult, ProxyMessage, ToLayer},
        proxies::outgoing::{OutgoingProxy, OutgoingProxyError, OutgoingProxyMessage},
        session_monitor::chaos::ChaosWatcherRx,
    };

    /// Verifies that the outgoing proxy can handle operator reconnect
    /// when there is an open connection.
    #[tokio::test]
    async fn clear_on_reconnect() {
        let peer_addr = "1.1.1.1:80".parse::<SocketAddr>().unwrap();
        let (connection, _, out) = Connection::dummy();

        let (_, chaos_rx) = watch::channel(Default::default());

        let mut background_tasks: BackgroundTasks<(), ProxyMessage, OutgoingProxyError> =
            BackgroundTasks::new(connection.tx_handle());

        let outgoing = background_tasks.register(
            OutgoingProxy::new(false, false, 0, 0, ChaosWatcherRx::new(chaos_rx)),
            (),
            8,
        );

        for i in 0..=1 {
            // Layer wants to make an outgoing connection.
            outgoing
                .send(OutgoingProxyMessage::Layer(
                    OutgoingRequest::Connect(OutgoingConnectRequest {
                        remote_address: SocketAddress::Ip(peer_addr),
                        protocol: NetProtocol::Stream,
                        metadata: OutgoingConnectRequestMetadata::default(),
                    }),
                    i,
                    LayerId(0),
                ))
                .await;
            let message = out.next().await.unwrap();
            assert_eq!(
                message,
                ClientMessage::TcpOutgoing(LayerTcpOutgoing::Connect(LayerConnect {
                    remote_address: SocketAddress::Ip(peer_addr),
                })),
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
                    message: ProxyToLayerMessage::Outgoing(OutgoingResponse::Connect(Ok(..))),
                }) => {
                    assert_eq!(message_id, i);
                }
                other => panic!("unexpected message from outgoing proxy: {other:?}"),
            }

            // Connection with the operator was reset.
            outgoing
                .send(OutgoingProxyMessage::ConnectionRefresh(
                    ConnectionRefresh::Start,
                ))
                .await;

            outgoing
                .send(OutgoingProxyMessage::ConnectionRefresh(
                    ConnectionRefresh::End(connection.tx_handle()),
                ))
                .await;
        }

        std::mem::drop(outgoing);
        match background_tasks.next().await.unwrap() {
            ((), TaskUpdate::Finished(Ok(()))) => {}
            other => panic!("unexpected update from the outgoing proxy: {other:?}"),
        }
    }

    /// Drives an [`OutgoingProxy`] with DNS filtering enabled, up to the point where the
    /// application has a UDP socket connected to the proxy and can send queries into it.
    ///
    /// Returns the pieces needed to keep driving it: the proxy's message sender, its task
    /// registry, the stream of messages sent to the agent, and the application's socket.
    async fn dns_filtering_setup() -> (
        TaskSender<OutgoingProxy>,
        BackgroundTasks<(), ProxyMessage, OutgoingProxyError>,
        ConnectionOutput<Client>,
        UdpSocket,
    ) {
        let (connection, _, to_agent) = Connection::dummy();
        let (_, chaos_rx) = watch::channel(Default::default());

        let mut background_tasks: BackgroundTasks<(), ProxyMessage, OutgoingProxyError> =
            BackgroundTasks::new(connection.tx_handle());
        let outgoing = background_tasks.register(
            OutgoingProxy::new(false, true, 0, 0, ChaosWatcherRx::new(chaos_rx)),
            (),
            8,
        );

        outgoing
            .send(OutgoingProxyMessage::Layer(
                OutgoingRequest::Connect(OutgoingConnectRequest {
                    remote_address: SocketAddress::Ip("1.2.3.4:53".parse().unwrap()),
                    protocol: NetProtocol::Datagrams,
                    metadata: OutgoingConnectRequestMetadata::default(),
                }),
                0,
                LayerId(0),
            ))
            .await;

        let layer_address = match background_tasks.next().await.unwrap().1.unwrap_message() {
            ProxyMessage::ToLayer(ToLayer {
                message: ProxyToLayerMessage::Outgoing(OutgoingResponse::Connect(Ok(response))),
                ..
            }) => match response.layer_address {
                SocketAddress::Ip(addr) => addr,
                other => panic!("expected an IP address, found {other:?}"),
            },
            other => panic!("unexpected message from outgoing proxy: {other:?}"),
        };

        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        socket
            .connect(SocketAddr::new(
                Ipv4Addr::LOCALHOST.into(),
                layer_address.port(),
            ))
            .await
            .unwrap();

        (outgoing, background_tasks, to_agent, socket)
    }

    fn dns_query(name: &str, record_type: RecordType) -> Vec<u8> {
        let mut message = Message::query();
        message.metadata.id = 42;
        message.add_query(Query::query(Name::from_ascii(name).unwrap(), record_type));
        message.to_vec().unwrap()
    }

    /// An `A` query sent straight to a DNS server is answered by the proxy, from a remote
    /// lookup, without ever opening a connection to that server.
    #[tokio::test]
    async fn dns_filtering_answers_address_queries_itself() {
        let (outgoing, mut background_tasks, to_agent, socket) = dns_filtering_setup().await;

        socket
            .send(&dns_query(
                "my-service.default.svc.cluster.local.",
                RecordType::A,
            ))
            .await
            .unwrap();

        let lookup = match background_tasks.next().await.unwrap().1.unwrap_message() {
            ProxyMessage::DnsFilteringLookup(lookup) => lookup,
            other => panic!("unexpected message from outgoing proxy: {other:?}"),
        };
        assert_eq!(lookup.request.node, "my-service.default.svc.cluster.local");

        outgoing
            .send(OutgoingProxyMessage::DnsFilteringLookupResult(
                DnsFilteringLookupResult {
                    id: lookup.id,
                    result: Ok(DnsLookup(vec![LookupRecord {
                        name: "my-service.default.svc.cluster.local".to_owned(),
                        ip: Ipv4Addr::new(10, 24, 0, 1).into(),
                    }])),
                },
            ))
            .await;

        let mut buffer = [0_u8; 512];
        let read = socket.recv(&mut buffer).await.unwrap();
        let response = Message::from_vec(buffer.get(..read).unwrap()).unwrap();

        assert_eq!(response.metadata.id, 42);
        assert_eq!(
            response.answers.first().unwrap().data,
            RData::A(A(Ipv4Addr::new(10, 24, 0, 1)))
        );

        // The whole exchange happened without bothering the agent about the DNS server.
        assert!(
            timeout(Duration::from_millis(100), to_agent.next())
                .await
                .is_err(),
            "no connection to the DNS server should have been made",
        );
    }

    /// A query we cannot resolve remotely makes the connection fall back to a plain tunnel, and
    /// the query that triggered it is forwarded rather than dropped.
    #[tokio::test]
    async fn dns_filtering_falls_back_to_tunneling_for_other_record_types() {
        let (_outgoing, _background_tasks, to_agent, socket) = dns_filtering_setup().await;

        let query = dns_query("_grpc._tcp.default.svc.cluster.local.", RecordType::SRV);
        socket.send(&query).await.unwrap();

        match to_agent.next().await.unwrap() {
            ClientMessage::UdpOutgoing(LayerUdpOutgoing::Connect(LayerConnect {
                remote_address,
            })) => {
                assert_eq!(
                    remote_address,
                    SocketAddress::Ip("1.2.3.4:53".parse().unwrap())
                );
            }
            other => panic!("expected a connect request to the DNS server, found {other:?}"),
        }
    }
}
