//! Handles the logic of the `outgoing` feature.

use std::{
    collections::HashMap,
    fmt, io,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    time::Instant,
};

use bytes::Bytes;
use mirrord_intproxy_protocol::{
    LayerId, MessageId, NetProtocol, OutgoingConnMetadataResponse, OutgoingConnectRequest,
    OutgoingConnectResponse, OutgoingRequest, OutgoingResponse, ProxyToLayerMessage,
};
use mirrord_protocol::{
    ConnectionId, DaemonMessage, RemoteResult, ResponseError,
    outgoing::{
        DaemonConnect, DaemonConnectV2, DaemonRead, OUTGOING_CONNECT_V2, SocketAddress,
        tcp::DaemonTcpOutgoing, udp::DaemonUdpOutgoing,
    },
    uid::Uid,
};
use semver::Version;
use thiserror::Error;
use tokio::net::TcpListener;
use tracing::Level;

use self::interceptor::Interceptor;
use crate::{
    ProxyMessage,
    background_tasks::{
        BackgroundTask, BackgroundTasks, MessageBus, TaskError, TaskSender, TaskUpdate,
    },
    error::{UnexpectedAgentMessage, agent_lost_io_error},
    main_tasks::{ConnectionRefresh, LayerClosed, LayerForked, ToLayer},
    proxies::outgoing::net_protocol_ext::{NetProtocolExt, PreparedSocket},
    remote_resources::RemoteResources,
    request_queue::RequestQueue,
    session_monitor::{MonitorEvent, MonitorTx, try_parse_http_request},
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
            message: ProxyToLayerMessage::Outgoing(OutgoingResponse::Connect(Err(error))),
        }
    }
}

#[derive(Debug)]
struct ConnectInProgress {
    prepared_listener: Option<TcpListener>,
    remote_address: SocketAddress,
    requested_at: Instant,
    layer_id: LayerId,
    message_id: MessageId,
    id: u128,
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

    /// [`TaskSender`]s for active [`Interceptor`] tasks.
    txs: HashMap<InterceptorId, TaskSender<Interceptor>>,
    /// For managing [`Interceptor`] tasks.
    background_tasks: Option<BackgroundTasks<InterceptorId, Bytes, io::Error>>,

    /// Whether TCP connect requests should be handled in a non-blocking way.
    ///
    /// See struct level docs for more info.
    non_blocking_tcp_connect: bool,
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

    /// Session monitor event sender.
    monitor_tx: MonitorTx,
    /// TCP connections that have not yet sent their first bytes to the agent.
    /// Used to sniff HTTP requests from the first data chunk.
    /// Stores the remote address so we can report the port in the event.
    http_sniff_pending: HashMap<InterceptorId, SocketAddress>,
}

impl OutgoingProxy {
    /// Used when registering new [`Interceptor`] tasks in the [`BackgroundTasks`] struct.
    const CHANNEL_SIZE: usize = 512;

    /// Creates a new instance, ready to run.
    ///
    /// # Params
    ///
    /// * `non_blocking_tcp_connect` - see struct level docs
    /// * `receive_delay_ms` - delay in milliseconds for receive operations (Agent → Layer)
    /// * `transmit_delay_ms` - delay in milliseconds for transmit operations (Layer → Agent)
    /// * `monitor_tx` - session monitor event sender
    pub fn new(
        non_blocking_tcp_connect: bool,
        receive_delay_ms: u64,
        transmit_delay_ms: u64,
        monitor_tx: MonitorTx,
    ) -> Self {
        Self {
            datagrams_reqs: Default::default(),
            stream_reqs: Default::default(),
            v2_reqs: Default::default(),
            txs: Default::default(),
            background_tasks: Default::default(),
            non_blocking_tcp_connect,
            protocol_version: Default::default(),
            receive_delay_ms,
            transmit_delay_ms,
            connections_in_layers: Default::default(),
            agent_local_addresses: Default::default(),
            monitor_tx,
            http_sniff_pending: Default::default(),
        }
    }

    /// If `id` has a pending HTTP sniff and `bytes` contains a valid HTTP/1.x request,
    /// emits a [`MonitorEvent::HttpRequest`].
    fn try_sniff_http(&mut self, id: &InterceptorId, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let Some(remote_addr) = self.http_sniff_pending.remove(id) else {
            return;
        };
        let Some((method, path, host)) = try_parse_http_request(bytes) else {
            return;
        };
        let port = match &remote_addr {
            SocketAddress::Ip(addr) => addr.port(),
            _ => 0,
        };
        self.monitor_tx.emit(MonitorEvent::HttpRequest {
            method,
            path,
            host,
            port,
        });
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

        // Apply receive delay if configured
        if self.receive_delay_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(self.receive_delay_ms)).await;
        }

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
            let message = match (uid, protocol) {
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
            };
            return Err(UnexpectedAgentMessage(message.into()).into());
        };

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
        let interceptor = self.background_tasks.as_mut().unwrap().register(
            Interceptor::new(id, prepared_socket),
            id,
            Self::CHANNEL_SIZE,
        );
        self.txs.insert(id, interceptor);

        // Register for HTTP sniffing on TCP connections.
        if protocol == NetProtocol::Stream {
            self.http_sniff_pending
                .insert(id, in_progress.remote_address);
        }

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
        // The chance for collision here is negligible.
        let connection_id = rand::random::<u128>();
        self.connections_in_layers.add(session_id, connection_id);

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

        let uid = if self
            .protocol_version
            .as_ref()
            .is_some_and(|version| OUTGOING_CONNECT_V2.matches(version))
        {
            let request_uid = Uid::new_v4();
            self.v2_reqs.insert(
                (request_uid, request.protocol),
                ConnectInProgress {
                    prepared_listener: prepared_stream,
                    remote_address: request.remote_address.clone(),
                    requested_at: Instant::now(),
                    layer_id: session_id,
                    message_id,
                    id: connection_id,
                },
            );
            Some(request_uid)
        } else {
            self.queue(request.protocol).push_back_with_data(
                message_id,
                session_id,
                ConnectInProgress {
                    id: connection_id,
                    prepared_listener: prepared_stream,
                    remote_address: request.remote_address.clone(),
                    requested_at: Instant::now(),
                    layer_id: session_id,
                    message_id,
                },
            );
            None
        };

        let msg = request
            .protocol
            .wrap_agent_connect(request.remote_address, uid);
        message_bus.send_agent(msg).await;

        Ok(())
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
                self.txs.clear();
                self.http_sniff_pending.clear();
                self.background_tasks.as_mut().unwrap().clear();
                self.protocol_version = None;

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

                tracing::debug!(
                    responses = self.v2_reqs.len(),
                    "Flushing error responses to V2 connect requests"
                );
                for in_progress in std::mem::take(&mut self.v2_reqs).into_values() {
                    message_bus
                        .send(ToLayer::from(AgentLostOutgoingResponse(
                            in_progress.layer_id,
                            in_progress.message_id,
                        )))
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
    AgentProtocolVersion(Version),
    Layer(OutgoingRequest, MessageId, LayerId),
    ConnectionRefresh(ConnectionRefresh),
    LayerForked(LayerForked),
    LayerClosed(LayerClosed),
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
                            let id = InterceptorId { connection_id: close, protocol: NetProtocol::Datagrams};
                            self.txs.remove(&id);
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
                    Some(OutgoingProxyMessage::Layer(request, message_id, layer_id)) => {
                        self.handle_layer_request(request, layer_id, message_id, message_bus).await?;
                    }
                    Some(OutgoingProxyMessage::LayerForked(forked)) => {
                        self.connections_in_layers.clone_all(forked.parent, forked.child);
                    }
                    Some(OutgoingProxyMessage::LayerClosed(closed)) => {
                        for id in self.connections_in_layers.remove_all(closed.id) {
                            self.agent_local_addresses.remove(&id);
                        }
                    }
                    Some(OutgoingProxyMessage::ConnectionRefresh(refresh)) => self.handle_connection_refresh(message_bus, refresh).await,
                    Some(OutgoingProxyMessage::AgentProtocolVersion(version)) => {
                        self.protocol_version.replace(version);
                    }
                },

                Some(task_update) = self.background_tasks.as_mut().unwrap().next() => match task_update {
                    (id, TaskUpdate::Message(bytes)) => {
                        // Apply transmit delay if configured
                        if self.transmit_delay_ms > 0 {
                            tokio::time::sleep(std::time::Duration::from_millis(self.transmit_delay_ms)).await;
                        }

                        // On the first non-empty data chunk from a TCP connection, try to
                        // detect an HTTP/1.x request and emit a monitor event.
                        self.try_sniff_http(&id, &bytes);

                        let msg = id.protocol.wrap_agent_write(id.connection_id, bytes);
                        message_bus.send_agent(msg).await;
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

                        self.http_sniff_pending.remove(&id);

                        if self.txs.remove(&id).is_some() {
                            tracing::trace!(%id, "Local connection closed, notifying the agent");
                            let msg = id.protocol.wrap_agent_close(id.connection_id);
                            let _ = message_bus.send_agent(msg).await;
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
        LayerId, NetProtocol, OutgoingConnectRequest, OutgoingRequest, OutgoingResponse,
        ProxyToLayerMessage,
    };
    use mirrord_protocol::{
        ClientMessage,
        outgoing::{
            DaemonConnect, LayerConnect, SocketAddress,
            tcp::{DaemonTcpOutgoing, LayerTcpOutgoing},
        },
    };
    use mirrord_protocol_io::Connection;

    use crate::{
        background_tasks::{BackgroundTasks, TaskUpdate},
        main_tasks::{ConnectionRefresh, ProxyMessage, ToLayer},
        proxies::outgoing::{OutgoingProxy, OutgoingProxyError, OutgoingProxyMessage},
        session_monitor::MonitorTx,
    };

    /// Verifies that the outgoing proxy can handle operator reconnect
    /// when there is an open connection.
    #[tokio::test]
    async fn clear_on_reconnect() {
        let peer_addr = "1.1.1.1:80".parse::<SocketAddr>().unwrap();
        let (connection, _, out) = Connection::dummy();

        let mut background_tasks: BackgroundTasks<(), ProxyMessage, OutgoingProxyError> =
            BackgroundTasks::new(connection.tx_handle());
        let outgoing = background_tasks.register(
            OutgoingProxy::new(false, 0, 0, MonitorTx::disabled()),
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
}
