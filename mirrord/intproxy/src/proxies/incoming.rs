//! Handles the logic of the `incoming` feature.
//!
//!
//! Background tasks:
//! 1. TcpProxy - always handles remote connection first. Attempts to connect a couple times. Waits
//!    until connection becomes readable (is TCP) or receives an http request.
//! 2. HttpSender -

use std::{collections::HashMap, io, net::SocketAddr, ops::Not, sync::Arc, time::Duration};

use bound_socket::BoundTcpSocket;
use http::{ClientStore, ResponseMode, StreamingBody};
use http_gateway::HttpGatewayTask;
use metadata_store::MetadataStore;
use mirrord_config::feature::network::incoming::https_delivery::LocalHttpsDelivery;
use mirrord_intproxy_protocol::{
    ConnMetadataRequest, ConnMetadataResponse, IncomingRequest, IncomingResponse, LayerId,
    MessageId, ProxyToLayerMessage,
};
use mirrord_protocol::{
    tcp::{
        ChunkedRequest, ChunkedRequestBodyV1, ChunkedRequestErrorV1, ChunkedRequestErrorV2,
        ChunkedResponse, DaemonTcp, HttpRequest, HttpRequestMetadata, InternalHttpBodyFrame,
        InternalHttpRequest, LayerTcp, LayerTcpSteal, NewTcpConnection, TcpData,
        TrafficTransportType,
    },
    ClientMessage, ConnectionId, RequestId, ResponseError,
};
use tasks::{HttpGatewayId, HttpOut, InProxyTask, InProxyTaskError, InProxyTaskMessage};
use tcp_proxy::{LocalTcpConnection, TcpProxyTask};
use thiserror::Error;
use tls::LocalTlsSetup;
use tokio::sync::mpsc;
use tracing::Level;

use self::subscriptions::SubscriptionsManager;
use crate::{
    background_tasks::{
        BackgroundTask, BackgroundTasks, MessageBus, TaskError, TaskSender, TaskUpdate,
    },
    main_tasks::{LayerClosed, LayerForked, ToLayer},
    ProxyMessage,
};

mod bound_socket;
mod http;
mod http_gateway;
mod metadata_store;
mod port_subscription_ext;
mod subscriptions;
mod tasks;
mod tcp_proxy;
mod tls;

#[derive(Default)]
struct MirrorOrSteal<T> {
    mirror: T,
    steal: T,
}

impl<T> MirrorOrSteal<T> {
    fn get(&self, is_steal: bool) -> &T {
        if is_steal {
            &self.steal
        } else {
            &self.mirror
        }
    }

    fn get_mut(&mut self, is_steal: bool) -> &mut T {
        if is_steal {
            &mut self.steal
        } else {
            &mut self.mirror
        }
    }

    fn for_both<F: Fn(&mut T)>(&mut self, f: F) {
        f(&mut self.mirror);
        f(&mut self.steal);
    }
}

/// Errors that can occur when handling the `incoming` feature.
#[derive(Error, Debug)]
pub enum IncomingProxyError {
    #[error("failed to prepare a TCP socket: {0}")]
    SocketSetupFailed(#[source] io::Error),
    #[error("subscribing port failed: {0}")]
    SubscriptionFailed(#[source] ResponseError),
}

/// Messages consumed by [`IncomingProxy`] running as a [`BackgroundTask`].
#[derive(Debug)]
pub enum IncomingProxyMessage {
    LayerRequest(MessageId, LayerId, IncomingRequest),
    LayerForked(LayerForked),
    LayerClosed(LayerClosed),
    AgentMirror(DaemonTcp),
    AgentSteal(DaemonTcp),
    /// Agent responded to [`ClientMessage::SwitchProtocolVersion`].
    AgentProtocolVersion(semver::Version),
    ConnectionRefresh,
}

/// Handle to a running [`HttpGatewayTask`].
struct HttpGatewayHandle {
    /// Only keeps the [`HttpGatewayTask`] alive.
    _tx: TaskSender<HttpGatewayTask>,
    /// For sending request body [`Frame`](hyper::body::Frame)s.
    ///
    /// [`None`] if all frames were already sent.
    body_tx: Option<mpsc::Sender<InternalHttpBodyFrame>>,
}

/// Handles logic and state of the `incoming` feature.
/// Run as a [`BackgroundTask`].
///
/// Handles port subscriptions state of the connected layers.
/// Utilizes multiple background tasks ([`TcpProxyTask`]s and [`HttpGatewayTask`]s) to handle
/// incoming connections and requests.
///
/// # Connections mirrored or stolen without a filter
///
/// Each such connection exists in two places:
///
/// 1. Here, between the intproxy and the user application. Managed by a single [`TcpProxyTask`].
/// 2. In the cluster, between the agent and the original TCP client.
///
/// We are notified about such connections with the [`NewTcpConnection`] message.
///
/// The local connection lives until the agent or the user application closes it, or a local IO
/// error occurs. When we want to close this connection, we simply drop the [`TcpProxyTask`]'s
/// [`TaskSender`]. When a local IO error occurs, the [`TcpProxyTask`] finishes with an
/// [`InProxyTaskError`].
///
/// # Requests stolen with a filter
///
/// In the cluster, we have a real persistent connection between the agent and the original HTTP
/// client. From this connection, intproxy receives a subset of requests.
///
/// Locally, we don't have a concept of a filered connection.
/// Each request is handled independently by a single [`HttpGatewayTask`].
/// Also:
/// 1. Local HTTP connections are reused when possible.
/// 2. Unless the error is fatal, each request is retried a couple of times.
/// 3. We never send [`LayerTcpSteal::ConnectionUnsubscribe`] (due to requests being handled
///    independently). If a request fails locally, we send a
///    [`StatusCode::BAD_GATEWAY`](hyper::http::StatusCode::BAD_GATEWAY) response.
///
/// We are notified about stolen requests with the [`HttpRequest`] messages.
///
/// The request can be cancelled only when one of the following happen:
/// 1. The agent closes the remote connection to which this request belongs
/// 2. The agent informs us that it failed to read request body ([`ChunkedRequest::ErrorV1`] or
///    [`ChunkedRequest::ErrorV2`])
///
/// When we want to cancel the request, we drop the [`HttpGatewayTask`]'s [`TaskSender`].
///
/// # HTTP upgrades
///
/// An HTTP request stolen with a filter can result in an HTTP upgrade.
/// When this happens, the TCP connection is recovered and passed to a new [`TcpProxyTask`].
/// The TCP connection is then treated as stolen without a filter.
pub struct IncomingProxy {
    /// Active port subscriptions for all layers.
    subscriptions: SubscriptionsManager,
    /// For managing intercepted connections metadata.
    metadata_store: MetadataStore,
    /// What HTTP response flavor we produce.
    response_mode: ResponseMode,
    /// For making TLS connections to the user application.
    tls_setup: Option<Arc<LocalTlsSetup>>,
    /// Cache for [`LocalHttpClient`](http::LocalHttpClient)s.
    client_store: ClientStore,
    /// Each mirrored/stolen remote connection is mapped to a [`TcpProxyTask`] here.
    ///
    /// Each entry here maps to a connection that is in progress both locally and remotely.
    tcp_proxies: MirrorOrSteal<HashMap<ConnectionId, TaskSender<TcpProxyTask>>>,
    /// Each mirrored/stolen remote HTTP request is mapped to a [`HttpGatewayTask`] here.
    ///
    /// Each entry here maps to a request that is in progress both locally and remotely.
    http_gateways: MirrorOrSteal<HashMap<ConnectionId, HashMap<RequestId, HttpGatewayHandle>>>,
    /// Running [`BackgroundTask`]s utilized by this proxy.
    tasks: BackgroundTasks<InProxyTask, InProxyTaskMessage, InProxyTaskError>,
}

impl IncomingProxy {
    /// Used when registering new tasks in the internal [`BackgroundTasks`] instance.
    const CHANNEL_SIZE: usize = 512;

    pub fn new(
        idle_local_http_connection_timeout: Duration,
        https_delivery: LocalHttpsDelivery,
    ) -> Self {
        let tls_setup = LocalTlsSetup::from_config(https_delivery).map(Arc::new);

        Self {
            subscriptions: Default::default(),
            metadata_store: Default::default(),
            response_mode: Default::default(),
            tls_setup: tls_setup.clone(),
            client_store: ClientStore::new_with_timeout(
                idle_local_http_connection_timeout,
                tls_setup,
            ),
            tcp_proxies: Default::default(),
            http_gateways: Default::default(),
            tasks: Default::default(),
        }
    }

    /// Starts a new [`HttpGatewayTask`] to handle the given request.
    ///
    /// If we don't have a [`PortSubscription`] for the port, the task is not started.
    /// Instead, we respond immediately to the agent.
    #[tracing::instrument(
        level = Level::DEBUG,
        skip(self, message_bus),
        ret,
    )]
    async fn start_http_gateway(
        &mut self,
        request: HttpRequest<StreamingBody>,
        body_tx: Option<mpsc::Sender<InternalHttpBodyFrame>>,
        transport: TrafficTransportType,
        is_steal: bool,
        message_bus: &MessageBus<Self>,
    ) {
        tracing::info!(
            full_headers = ?request.internal_request.headers,
            ?request,
            ?transport,
            is_steal,
            "Received an HTTP request from the agent",
        );

        let subscription = self
            .subscriptions
            .get(request.port)
            .filter(|subscription| subscription.subscription.is_steal() == is_steal);
        let Some(subscription) = subscription else {
            tracing::debug!(
                "Received a new HTTP request within a stale port subscription, \
                sending an unsubscribe request or an error response."
            );

            if is_steal.not() {
                return;
            }

            let no_other_requests = self
                .http_gateways
                .get(is_steal)
                .get(&request.connection_id)
                .map(|gateways| gateways.is_empty())
                .unwrap_or(true);
            if no_other_requests {
                message_bus
                    .send(ClientMessage::TcpSteal(
                        LayerTcpSteal::ConnectionUnsubscribe(request.connection_id),
                    ))
                    .await;
            } else {
                let response = http::mirrord_error_response(
                    "port no longer subscribed",
                    request.version(),
                    request.connection_id,
                    request.request_id,
                    request.port,
                );
                message_bus
                    .send(ClientMessage::TcpSteal(LayerTcpSteal::HttpResponse(
                        response,
                    )))
                    .await;
            }

            return;
        };

        let connection_id = request.connection_id;
        let request_id = request.request_id;
        let id = HttpGatewayId {
            connection_id,
            request_id,
            port: request.port,
            version: request.version(),
        };
        let tx = self.tasks.register(
            HttpGatewayTask::new(
                request,
                self.client_store.clone(),
                self.response_mode,
                subscription.listening_on,
                transport,
            ),
            if is_steal {
                InProxyTask::StealHttpGateway(id)
            } else {
                InProxyTask::MirrorHttpGateway(id)
            },
            Self::CHANNEL_SIZE,
        );

        self.http_gateways
            .get_mut(is_steal)
            .entry(connection_id)
            .or_default()
            .insert(request_id, HttpGatewayHandle { _tx: tx, body_tx });
    }

    /// Handles [`NewTcpConnection`] message from the agent, starting a new [`TcpProxyTask`].
    ///
    /// If we don't have a [`PortSubscription`] for the port, the task is not started.
    /// Instead, we respond immediately to the agent.
    #[tracing::instrument(level = Level::TRACE, skip(self, message_bus))]
    async fn handle_new_connection(
        &mut self,
        connection: NewTcpConnection,
        transport: TrafficTransportType,
        is_steal: bool,
        message_bus: &mut MessageBus<Self>,
    ) -> Result<(), IncomingProxyError> {
        let NewTcpConnection {
            connection_id,
            remote_address,
            destination_port,
            source_port,
            local_address,
        } = connection;

        let subscription = self
            .subscriptions
            .get(destination_port)
            .filter(|subscription| subscription.subscription.is_steal() == is_steal);
        let Some(subscription) = subscription else {
            tracing::debug!(
                port = destination_port,
                connection_id,
                "Received a new connection within a stale port subscription, sending an unsubscribe request.",
            );

            let message = if is_steal {
                ClientMessage::Tcp(LayerTcp::ConnectionUnsubscribe(connection_id))
            } else {
                ClientMessage::TcpSteal(LayerTcpSteal::ConnectionUnsubscribe(connection_id))
            };
            message_bus.send(message).await;

            return Ok(());
        };

        let socket = BoundTcpSocket::bind_specified_or_localhost(subscription.listening_on.ip())
            .map_err(IncomingProxyError::SocketSetupFailed)?;

        self.metadata_store.expect(
            ConnMetadataRequest {
                listener_address: subscription.listening_on,
                peer_address: socket
                    .local_addr()
                    .map_err(IncomingProxyError::SocketSetupFailed)?,
            },
            connection_id,
            ConnMetadataResponse {
                remote_source: SocketAddr::new(remote_address, source_port),
                local_address,
            },
        );

        let id = if is_steal {
            InProxyTask::StealTcpProxy(connection_id)
        } else {
            InProxyTask::MirrorTcpProxy(connection_id)
        };
        let tx = self.tasks.register(
            TcpProxyTask::new(
                connection_id,
                LocalTcpConnection::FromTheStart {
                    socket,
                    peer: subscription.listening_on,
                    tls_setup: self.tls_setup.clone(),
                    transport,
                },
                is_steal.not(),
            ),
            id,
            Self::CHANNEL_SIZE,
        );

        self.tcp_proxies.get_mut(is_steal).insert(connection_id, tx);

        Ok(())
    }

    /// Handles [`ChunkedRequest`] message from the agent.
    async fn handle_chunked_request(
        &mut self,
        request: ChunkedRequest,
        is_steal: bool,
        message_bus: &mut MessageBus<Self>,
    ) {
        match request {
            ChunkedRequest::StartV1(request) => {
                let (body_tx, body_rx) = mpsc::channel(128);
                let request = request.map_body(|frames| StreamingBody::new(body_rx, frames));
                self.start_http_gateway(
                    request,
                    Some(body_tx),
                    TrafficTransportType::Tcp,
                    is_steal,
                    message_bus,
                )
                .await;
            }

            ChunkedRequest::StartV2(request) => {
                let (body, body_tx) = if request.request.body.is_last {
                    (StreamingBody::from(request.request.body.frames), None)
                } else {
                    let (body_tx, body_rx) = mpsc::channel(128);
                    (
                        StreamingBody::new(body_rx, request.request.body.frames),
                        Some(body_tx),
                    )
                };

                let transport = request.transport;

                let HttpRequestMetadata::V1 { destination, .. } = request.metadata;
                let request = HttpRequest {
                    connection_id: request.connection_id,
                    request_id: request.request_id,
                    internal_request: InternalHttpRequest {
                        method: request.request.method,
                        uri: request.request.uri,
                        headers: request.request.headers,
                        version: request.request.version,
                        body,
                    },
                    port: destination.port(),
                };

                self.start_http_gateway(request, body_tx, transport, is_steal, message_bus)
                    .await;
            }

            ChunkedRequest::Body(ChunkedRequestBodyV1 {
                frames,
                is_last,
                connection_id,
                request_id,
            }) => {
                let gateway = self
                    .http_gateways
                    .get_mut(is_steal)
                    .get_mut(&connection_id)
                    .and_then(|gateways| gateways.get_mut(&request_id));

                let Some(gateway) = gateway else {
                    tracing::debug!(
                        connection_id,
                        request_id,
                        frames = ?frames,
                        last_body_chunk = is_last,
                        "Received a body chunk for a request that is no longer alive locally"
                    );

                    return;
                };

                let Some(tx) = gateway.body_tx.as_ref() else {
                    tracing::debug!(
                        connection_id,
                        request_id,
                        frames = ?frames,
                        last_body_chunk = is_last,
                        "Received a body chunk for a request with a closed body"
                    );

                    return;
                };

                for frame in frames {
                    if let Err(err) = tx.send(frame).await {
                        tracing::debug!(
                            frame = ?err.0,
                            connection_id,
                            request_id,
                            "Failed to send an HTTP request body frame to the HttpGatewayTask, channel is closed"
                        );
                        break;
                    }
                }

                if is_last {
                    gateway.body_tx = None;
                }
            }

            ChunkedRequest::ErrorV1(ChunkedRequestErrorV1 {
                connection_id,
                request_id,
            }) => {
                tracing::debug!(
                    connection_id,
                    request_id,
                    "Received an error in an HTTP request body",
                );

                if let Some(gateways) = self.http_gateways.get_mut(is_steal).get_mut(&connection_id)
                {
                    gateways.remove(&request_id);
                };
            }

            ChunkedRequest::ErrorV2(ChunkedRequestErrorV2 {
                connection_id,
                request_id,
                error_message,
            }) => {
                tracing::debug!(
                    connection_id,
                    request_id,
                    error = error_message,
                    "Received an error in an HTTP request body",
                );

                if let Some(gateways) = self.http_gateways.get_mut(is_steal).get_mut(&connection_id)
                {
                    gateways.remove(&request_id);
                };
            }
        }
    }

    /// Handles all agent messages.
    async fn handle_agent_message(
        &mut self,
        message: DaemonTcp,
        is_steal: bool,
        message_bus: &mut MessageBus<Self>,
    ) -> Result<(), IncomingProxyError> {
        match message {
            DaemonTcp::Close(close) => {
                self.tcp_proxies
                    .get_mut(is_steal)
                    .remove(&close.connection_id);
                self.http_gateways
                    .get_mut(is_steal)
                    .remove(&close.connection_id);
            }

            DaemonTcp::Data(data) => {
                let tx = self.tcp_proxies.get(is_steal).get(&data.connection_id);
                if let Some(tx) = tx {
                    tx.send(data.bytes).await;
                } else {
                    tracing::debug!(
                        connection_id = data.connection_id,
                        bytes = data.bytes.len(),
                        "Received new data for a connection that does not belong to any TcpProxy task",
                    );
                }
            }

            DaemonTcp::HttpRequest(request) => {
                self.start_http_gateway(
                    request.map_body(From::from),
                    None,
                    TrafficTransportType::Tcp,
                    is_steal,
                    message_bus,
                )
                .await;
            }

            DaemonTcp::HttpRequestFramed(request) => {
                self.start_http_gateway(
                    request.map_body(From::from),
                    None,
                    TrafficTransportType::Tcp,
                    is_steal,
                    message_bus,
                )
                .await;
            }

            DaemonTcp::HttpRequestChunked(request) => {
                self.handle_chunked_request(request, is_steal, message_bus)
                    .await;
            }

            DaemonTcp::NewConnection(connection) => {
                self.handle_new_connection(
                    connection,
                    TrafficTransportType::Tcp,
                    is_steal,
                    message_bus,
                )
                .await?;
            }

            DaemonTcp::NewConnectionV2(connection) => {
                self.handle_new_connection(
                    connection.connection,
                    connection.transport,
                    is_steal,
                    message_bus,
                )
                .await?;
            }

            DaemonTcp::SubscribeResult(result) => {
                let msgs = self.subscriptions.agent_responded(result)?;

                for msg in msgs {
                    message_bus.send(msg).await;
                }
            }
        }

        Ok(())
    }

    /// Handles all messages from this task's [`MessageBus`].
    #[tracing::instrument(level = Level::TRACE, skip(self, message_bus), ret, err)]
    async fn handle_message(
        &mut self,
        message: IncomingProxyMessage,
        message_bus: &mut MessageBus<Self>,
    ) -> Result<(), IncomingProxyError> {
        match message {
            IncomingProxyMessage::LayerRequest(message_id, layer_id, req) => match req {
                IncomingRequest::PortSubscribe(subscribe) => {
                    let msg = self
                        .subscriptions
                        .layer_subscribed(layer_id, message_id, subscribe);

                    if let Some(msg) = msg {
                        message_bus.send(msg).await;
                    }
                }
                IncomingRequest::PortUnsubscribe(unsubscribe) => {
                    let msg = self.subscriptions.layer_unsubscribed(layer_id, unsubscribe);

                    if let Some(msg) = msg {
                        message_bus.send(msg).await;
                    }
                }
                IncomingRequest::ConnMetadata(req) => {
                    let res = self.metadata_store.get(req);
                    message_bus
                        .send(ToLayer {
                            message_id,
                            layer_id,
                            message: ProxyToLayerMessage::Incoming(IncomingResponse::ConnMetadata(
                                res,
                            )),
                        })
                        .await;
                }
            },

            IncomingProxyMessage::AgentMirror(msg) => {
                self.handle_agent_message(msg, false, message_bus).await?;
            }

            IncomingProxyMessage::AgentSteal(msg) => {
                self.handle_agent_message(msg, true, message_bus).await?;
            }

            IncomingProxyMessage::LayerClosed(msg) => {
                let msgs = self.subscriptions.layer_closed(msg.id);

                for msg in msgs {
                    message_bus.send(msg).await;
                }
            }

            IncomingProxyMessage::LayerForked(msg) => {
                self.subscriptions.layer_forked(msg.parent, msg.child);
            }

            IncomingProxyMessage::AgentProtocolVersion(version) => {
                self.response_mode = ResponseMode::from(&version);
            }

            IncomingProxyMessage::ConnectionRefresh => {
                self.tcp_proxies.for_both(HashMap::clear);
                self.http_gateways.for_both(HashMap::clear);
                self.tasks.clear();

                for subscription in self.subscriptions.iter_mut() {
                    tracing::info!(?subscription, "Resubscribing after connection refresh");

                    message_bus
                        .send(ProxyMessage::ToAgent(subscription.resubscribe_message()))
                        .await
                }
            }
        }

        Ok(())
    }

    /// Handles all updates from [`TcpProxyTask`]s.
    #[tracing::instrument(level = Level::TRACE, skip(self, message_bus), ret)]
    async fn handle_tcp_proxy_update(
        &mut self,
        connection_id: ConnectionId,
        is_steal: bool,
        update: TaskUpdate<InProxyTaskMessage, InProxyTaskError>,
        message_bus: &mut MessageBus<Self>,
    ) {
        match update {
            TaskUpdate::Finished(result) => {
                match result {
                    Err(TaskError::Error(error)) => {
                        tracing::warn!(connection_id, %error, is_steal, "TcpProxyTask failed");
                    }
                    Err(TaskError::Panic) => {
                        tracing::error!(connection_id, is_steal, "TcpProxyTask task panicked");
                    }
                    Ok(()) => {}
                };

                self.metadata_store.no_longer_expect(connection_id);

                let send_unsubscribe = self
                    .tcp_proxies
                    .get_mut(is_steal)
                    .remove(&connection_id)
                    .is_some();
                if send_unsubscribe && is_steal {
                    message_bus
                        .send(ClientMessage::TcpSteal(
                            LayerTcpSteal::ConnectionUnsubscribe(connection_id),
                        ))
                        .await;
                } else if send_unsubscribe {
                    message_bus
                        .send(ClientMessage::Tcp(LayerTcp::ConnectionUnsubscribe(
                            connection_id,
                        )))
                        .await;
                }
            }

            TaskUpdate::Message(InProxyTaskMessage::Tcp(bytes)) if is_steal => {
                if self.tcp_proxies.steal.contains_key(&connection_id) {
                    message_bus
                        .send(ClientMessage::TcpSteal(LayerTcpSteal::Data(TcpData {
                            connection_id,
                            bytes,
                        })))
                        .await;
                }
            }

            TaskUpdate::Message(InProxyTaskMessage::Tcp(..)) => {}

            TaskUpdate::Message(InProxyTaskMessage::Http(..)) => {
                unreachable!("TcpProxyTask does not produce HTTP messages")
            }
        }
    }

    /// Handles all updates from [`HttpGatewayTask`]s.
    #[tracing::instrument(level = Level::TRACE, skip(self, message_bus), ret)]
    async fn handle_http_gateway_update(
        &mut self,
        id: HttpGatewayId,
        is_steal: bool,
        update: TaskUpdate<InProxyTaskMessage, InProxyTaskError>,
        message_bus: &mut MessageBus<Self>,
    ) {
        match update {
            TaskUpdate::Finished(result) => {
                let respond_on_panic = is_steal
                    && self
                        .http_gateways
                        .steal
                        .get_mut(&id.connection_id)
                        .and_then(|gateways| gateways.remove(&id.request_id))
                        .is_some();

                match result {
                    Ok(()) => {}
                    Err(TaskError::Error(..)) => {
                        unreachable!("HttpGatewayTask does not return any errors")
                    }
                    Err(TaskError::Panic) => {
                        tracing::error!(
                            connection_id = id.connection_id,
                            request_id = id.request_id,
                            is_steal,
                            "HttpGatewayTask panicked",
                        );

                        if respond_on_panic {
                            let response = http::mirrord_error_response(
                                "HTTP gateway task panicked",
                                id.version,
                                id.connection_id,
                                id.request_id,
                                id.port,
                            );
                            message_bus
                                .send(ClientMessage::TcpSteal(LayerTcpSteal::HttpResponse(
                                    response,
                                )))
                                .await;
                        }
                    }
                }
            }

            TaskUpdate::Message(InProxyTaskMessage::Http(message)) => {
                let exists = self
                    .http_gateways
                    .get(is_steal)
                    .get(&id.connection_id)
                    .and_then(|gateways| gateways.get(&id.request_id))
                    .is_some();
                if exists.not() {
                    return;
                }

                match message {
                    HttpOut::ResponseBasic(response) => {
                        tracing::info!(
                            full_headers = ?response.internal_response.headers,
                            ?response,
                            is_steal,
                            "Received an HTTP response from an HttpGatewayTask",
                        );

                        if is_steal {
                            message_bus
                                .send(ClientMessage::TcpSteal(LayerTcpSteal::HttpResponse(
                                    response,
                                )))
                                .await
                        }
                    }
                    HttpOut::ResponseFramed(response) => {
                        tracing::info!(
                            full_headers = ?response.internal_response.headers,
                            ?response,
                            is_steal,
                            "Received an HTTP response from an HttpGatewayTask",
                        );

                        if is_steal {
                            message_bus
                                .send(ClientMessage::TcpSteal(LayerTcpSteal::HttpResponseFramed(
                                    response,
                                )))
                                .await
                        }
                    }
                    HttpOut::ResponseChunked(response) => {
                        if let ChunkedResponse::Start(start) = &response {
                            tracing::info!(
                                full_headers = ?start.internal_response.headers,
                                response = ?start,
                                is_steal,
                                "Received an HTTP response from an HttpGatewayTask",
                            );
                        }

                        if is_steal {
                            message_bus
                                .send(ClientMessage::TcpSteal(LayerTcpSteal::HttpResponseChunked(
                                    response,
                                )))
                                .await;
                        }
                    }
                    HttpOut::Upgraded(on_upgrade) => {
                        let proxy = self.tasks.register(
                            TcpProxyTask::new(
                                id.connection_id,
                                LocalTcpConnection::AfterUpgrade(on_upgrade),
                                is_steal.not(),
                            ),
                            InProxyTask::StealTcpProxy(id.connection_id),
                            Self::CHANNEL_SIZE,
                        );

                        self.tcp_proxies
                            .get_mut(is_steal)
                            .insert(id.connection_id, proxy);
                    }
                }
            }

            TaskUpdate::Message(InProxyTaskMessage::Tcp(..)) => {
                unreachable!("HttpGatewayTask does not produce TCP messages")
            }
        }
    }
}

impl BackgroundTask for IncomingProxy {
    type Error = IncomingProxyError;
    type MessageIn = IncomingProxyMessage;
    type MessageOut = ProxyMessage;

    #[tracing::instrument(level = Level::INFO, name = "incoming_proxy_main_loop", skip_all, err)]
    async fn run(&mut self, message_bus: &mut MessageBus<Self>) -> Result<(), Self::Error> {
        loop {
            tokio::select! {
                msg = message_bus.recv() => match msg {
                    None => {
                        tracing::debug!("Message bus closed, exiting");
                        break Ok(());
                    },
                    Some(message) => self.handle_message(message, message_bus).await?,
                },

                Some((id, update)) = self.tasks.next() => match id {
                    InProxyTask::MirrorTcpProxy(connection_id) => {
                        self.handle_tcp_proxy_update(connection_id, false, update, message_bus).await;
                    }
                    InProxyTask::StealTcpProxy(connection_id) => {
                        self.handle_tcp_proxy_update(connection_id, true, update, message_bus).await;
                    }
                    InProxyTask::MirrorHttpGateway(id) => {
                        self.handle_http_gateway_update(id, false, update, message_bus).await;
                    }
                    InProxyTask::StealHttpGateway(id) => {
                        self.handle_http_gateway_update(id, true, update, message_bus).await;
                    }
                },
            }
        }
    }
}
