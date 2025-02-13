//! Handles the logic of the `incoming` feature.
//!
//!
//! Background tasks:
//! 1. TcpProxy - always handles remote connection first. Attempts to connect a couple times. Waits
//!    until connection becomes readable (is TCP) or receives an http request.
//! 2. HttpSender -

use std::{collections::HashMap, io, net::SocketAddr, time::Duration};

use bound_socket::BoundTcpSocket;
use http::{ClientStore, ResponseMode, StreamingBody};
use http_gateway::HttpGatewayTask;
use metadata_store::MetadataStore;
use mirrord_intproxy_protocol::{
    ConnMetadataRequest, ConnMetadataResponse, IncomingRequest, IncomingResponse, LayerId,
    MessageId, PortSubscription, ProxyToLayerMessage,
};
use mirrord_protocol::{
    tcp::{
        ChunkedHttpBody, ChunkedHttpError, ChunkedRequest, DaemonTcp, HttpRequest,
        InternalHttpBodyFrame, LayerTcp, LayerTcpSteal, NewTcpConnection, StealType, TcpData,
    },
    ClientMessage, ConnectionId, RequestId, ResponseError,
};
use tasks::{HttpGatewayId, HttpOut, InProxyTask, InProxyTaskError, InProxyTaskMessage};
use tcp_proxy::{LocalTcpConnection, TcpProxyTask};
use thiserror::Error;
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
/// 2. The agent informs us that it failed to read request body ([`ChunkedRequest::Error`])
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
    /// Cache for [`LocalHttpClient`](http::LocalHttpClient)s.
    client_store: ClientStore,
    /// Each mirrored remote connection is mapped to a [`TcpProxyTask`] in mirror mode.
    ///
    /// Each entry here maps to a connection that is in progress both locally and remotely.
    mirror_tcp_proxies: HashMap<ConnectionId, TaskSender<TcpProxyTask>>,
    /// Each remote connection stolen without a filter is mapped to a [`TcpProxyTask`] in steal
    /// mode.
    ///
    /// Each entry here maps to a connection that is in progress both locally and remotely.
    steal_tcp_proxies: HashMap<ConnectionId, TaskSender<TcpProxyTask>>,
    /// Each remote HTTP request stolen with a filter is mapped to a [`HttpGatewayTask`].
    ///
    /// Each entry here maps to a request that is in progress both locally and remotely.
    http_gateways: HashMap<ConnectionId, HashMap<RequestId, HttpGatewayHandle>>,
    /// Running [`BackgroundTask`]s utilized by this proxy.
    tasks: BackgroundTasks<InProxyTask, InProxyTaskMessage, InProxyTaskError>,
}

impl IncomingProxy {
    /// Used when registering new tasks in the internal [`BackgroundTasks`] instance.
    const CHANNEL_SIZE: usize = 512;

    pub fn new(idle_local_http_connection_timeout: Duration) -> Self {
        Self {
            subscriptions: Default::default(),
            metadata_store: Default::default(),
            response_mode: Default::default(),
            client_store: ClientStore::new_with_timeout(idle_local_http_connection_timeout),
            mirror_tcp_proxies: Default::default(),
            steal_tcp_proxies: Default::default(),
            http_gateways: Default::default(),
            tasks: Default::default(),
        }
    }

    /// Starts a new [`HttpGatewayTask`] to handle the given request.
    ///
    /// If we don't have a [`PortSubscription`] for the port, the task is not started.
    /// Instead, we respond immediately to the agent.
    #[tracing::instrument(level = Level::TRACE, skip(self, message_bus))]
    async fn start_http_gateway(
        &mut self,
        request: HttpRequest<StreamingBody>,
        body_tx: Option<mpsc::Sender<InternalHttpBodyFrame>>,
        message_bus: &MessageBus<Self>,
    ) {
        let subscription = self.subscriptions.get(request.port).filter(|subscription| {
            matches!(
                subscription.subscription,
                PortSubscription::Steal(
                    StealType::FilteredHttp(..) | StealType::FilteredHttpEx(..)
                )
            )
        });
        let Some(subscription) = subscription else {
            tracing::debug!(
                ?request,
                "Received a new HTTP request within a stale port subscription, \
                sending an unsubscribe request or an error response."
            );

            let no_other_requests = self
                .http_gateways
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
                    "port no longer subscribed with an HTTP filter",
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
            ),
            InProxyTask::HttpGateway(id),
            Self::CHANNEL_SIZE,
        );
        self.http_gateways
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
            .filter(|subscription| match &subscription.subscription {
                PortSubscription::Mirror(..) if !is_steal => true,
                PortSubscription::Steal(StealType::All(..)) if is_steal => true,
                _ => false,
            });
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
                LocalTcpConnection::FromTheStart {
                    socket,
                    peer: subscription.listening_on,
                },
                !is_steal,
            ),
            id,
            Self::CHANNEL_SIZE,
        );

        if is_steal {
            self.steal_tcp_proxies.insert(connection_id, tx);
        } else {
            self.mirror_tcp_proxies.insert(connection_id, tx);
        }

        Ok(())
    }

    /// Handles [`ChunkedRequest`] message from the agent.
    async fn handle_chunked_request(
        &mut self,
        request: ChunkedRequest,
        message_bus: &mut MessageBus<Self>,
    ) {
        match request {
            ChunkedRequest::Start(request) => {
                let (body_tx, body_rx) = mpsc::channel(128);
                let request = request.map_body(|frames| StreamingBody::new(body_rx, frames));
                self.start_http_gateway(request, Some(body_tx), message_bus)
                    .await;
            }

            ChunkedRequest::Body(ChunkedHttpBody {
                frames,
                is_last,
                connection_id,
                request_id,
            }) => {
                let gateway = self
                    .http_gateways
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

            ChunkedRequest::Error(ChunkedHttpError {
                connection_id,
                request_id,
            }) => {
                tracing::debug!(
                    connection_id,
                    request_id,
                    "Received an error in an HTTP request body",
                );

                if let Some(gateways) = self.http_gateways.get_mut(&connection_id) {
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
                if is_steal {
                    self.steal_tcp_proxies.remove(&close.connection_id);
                    self.http_gateways.remove(&close.connection_id);
                } else {
                    self.mirror_tcp_proxies.remove(&close.connection_id);
                }
            }

            DaemonTcp::Data(data) => {
                let tx = if is_steal {
                    self.steal_tcp_proxies.get(&data.connection_id)
                } else {
                    self.mirror_tcp_proxies.get(&data.connection_id)
                };

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
                self.start_http_gateway(request.map_body(From::from), None, message_bus)
                    .await;
            }

            DaemonTcp::HttpRequestFramed(request) => {
                self.start_http_gateway(request.map_body(From::from), None, message_bus)
                    .await;
            }

            DaemonTcp::HttpRequestChunked(request) => {
                self.handle_chunked_request(request, message_bus).await;
            }

            DaemonTcp::NewConnection(connection) => {
                self.handle_new_connection(connection, is_steal, message_bus)
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
    #[tracing::instrument(level = Level::TRACE, skip(self, message_bus), err)]
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
        }

        Ok(())
    }

    /// Handles all updates from [`TcpProxyTask`]s.
    #[tracing::instrument(level = Level::TRACE, skip(self, message_bus))]
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

                if is_steal {
                    if self.steal_tcp_proxies.remove(&connection_id).is_some() {
                        message_bus
                            .send(ClientMessage::TcpSteal(
                                LayerTcpSteal::ConnectionUnsubscribe(connection_id),
                            ))
                            .await;
                    }
                } else if self.mirror_tcp_proxies.remove(&connection_id).is_some() {
                    message_bus
                        .send(ClientMessage::Tcp(LayerTcp::ConnectionUnsubscribe(
                            connection_id,
                        )))
                        .await;
                }
            }

            TaskUpdate::Message(..) if !is_steal => {
                unreachable!("TcpProxyTask does not produce messages in mirror mode")
            }

            TaskUpdate::Message(InProxyTaskMessage::Tcp(bytes)) => {
                if self.steal_tcp_proxies.contains_key(&connection_id) {
                    message_bus
                        .send(ClientMessage::TcpSteal(LayerTcpSteal::Data(TcpData {
                            connection_id,
                            bytes,
                        })))
                        .await;
                }
            }

            TaskUpdate::Message(InProxyTaskMessage::Http(..)) => {
                unreachable!("TcpProxyTask does not produce HTTP messages")
            }
        }
    }

    /// Handles all updates from [`HttpGatewayTask`]s.
    #[tracing::instrument(level = Level::TRACE, skip(self, message_bus))]
    async fn handle_http_gateway_update(
        &mut self,
        id: HttpGatewayId,
        update: TaskUpdate<InProxyTaskMessage, InProxyTaskError>,
        message_bus: &mut MessageBus<Self>,
    ) {
        match update {
            TaskUpdate::Finished(result) => {
                let respond_on_panic = self
                    .http_gateways
                    .get_mut(&id.connection_id)
                    .and_then(|gateways| gateways.remove(&id.request_id))
                    .is_some();

                match result {
                    Ok(()) => {}
                    Err(TaskError::Error(
                        InProxyTaskError::IoError(..) | InProxyTaskError::UpgradeError(..),
                    )) => unreachable!("HttpGatewayTask does not return any errors"),
                    Err(TaskError::Panic) => {
                        tracing::error!(
                            connection_id = id.connection_id,
                            request_id = id.request_id,
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
                    .get(&id.connection_id)
                    .and_then(|gateways| gateways.get(&id.request_id))
                    .is_some();
                if !exists {
                    return;
                }

                match message {
                    HttpOut::ResponseBasic(response) => {
                        message_bus
                            .send(ClientMessage::TcpSteal(LayerTcpSteal::HttpResponse(
                                response,
                            )))
                            .await
                    }
                    HttpOut::ResponseFramed(response) => {
                        message_bus
                            .send(ClientMessage::TcpSteal(LayerTcpSteal::HttpResponseFramed(
                                response,
                            )))
                            .await
                    }
                    HttpOut::ResponseChunked(response) => {
                        message_bus
                            .send(ClientMessage::TcpSteal(LayerTcpSteal::HttpResponseChunked(
                                response,
                            )))
                            .await;
                    }
                    HttpOut::Upgraded(on_upgrade) => {
                        let proxy = self.tasks.register(
                            TcpProxyTask::new(LocalTcpConnection::AfterUpgrade(on_upgrade), false),
                            InProxyTask::StealTcpProxy(id.connection_id),
                            Self::CHANNEL_SIZE,
                        );
                        self.steal_tcp_proxies.insert(id.connection_id, proxy);
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

    #[tracing::instrument(level = Level::TRACE, name = "incoming_proxy_main_loop", skip_all, err)]
    async fn run(mut self, message_bus: &mut MessageBus<Self>) -> Result<(), Self::Error> {
        loop {
            tokio::select! {
                msg = message_bus.recv() => match msg {
                    None => {
                        tracing::trace!("message bus closed, exiting");
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
                    InProxyTask::HttpGateway(id) => {
                        self.handle_http_gateway_update(id, update, message_bus).await;
                    }
                },
            }
        }
    }
}
