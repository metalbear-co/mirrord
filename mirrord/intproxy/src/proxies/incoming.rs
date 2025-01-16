//! Handles the logic of the `incoming` feature.
//!
//!
//! Background tasks:
//! 1. TcpProxy - always handles remote connection first. Attempts to connect a couple times. Waits
//!    until connection becomes readable (is TCP) or receives an http request.
//! 2. HttpSender -

use std::{
    collections::{hash_map::Entry, HashMap},
    io,
    net::SocketAddr,
};

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
pub mod port_subscription_ext;
mod subscriptions;
mod tasks;
mod tcp_proxy;

/// Errors that can occur when handling the `incoming` feature.
#[derive(Error, Debug)]
pub enum IncomingProxyError {
    #[error("failed to prepare a TPC socket: {0}")]
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

struct HttpGatewayHandle {
    _tx: TaskSender<HttpGatewayTask>,
    body_tx: Option<mpsc::Sender<InternalHttpBodyFrame>>,
}

/// Handles logic and state of the `incoming` feature.
/// Run as a [`BackgroundTask`].
///
/// Handles port subscriptions state of the connected layers. Utilizes multiple background tasks
/// ([`Interceptor`]s and [`HttpResponseReader`]s) to handle incoming connections.
///
/// Each connection is managed by a single [`Interceptor`],
/// that establishes a TCP connection with the user application's port and proxies data.
///
/// Bodies of HTTP responses from the user application are polled by [`HttpResponseReader`]s.
///
/// Incoming connections are created by the agent either explicitly ([`NewTcpConnection`] message)
/// or implicitly ([`HttpRequest`](mirrord_protocol::tcp::HttpRequest)).
#[derive(Default)]
pub struct IncomingProxy {
    /// Active port subscriptions for all layers.
    subscriptions: SubscriptionsManager,
    /// For managing intercepted connections metadata.
    metadata_store: MetadataStore,
    response_mode: ResponseMode,
    /// Cache for [`LocalHttpClient`](http::LocalHttpClient)s.
    client_store: ClientStore,
    /// Each mirrored remote connection is mapped to a [TcpProxyTask] in mirror mode.
    mirror_tcp_proxies: HashMap<ConnectionId, TaskSender<TcpProxyTask>>,
    /// Each remote connection stolen in whole is mapped to a [TcpProxyTask] in steal mode.
    steal_tcp_proxies: HashMap<ConnectionId, TaskSender<TcpProxyTask>>,
    /// Each remote connection stolen with a filter is mapped to [HttpGatewayTask]s.
    http_gateways: HashMap<ConnectionId, HashMap<RequestId, HttpGatewayHandle>>,
    tasks: BackgroundTasks<InProxyTask, InProxyTaskMessage, InProxyTaskError>,
}

impl IncomingProxy {
    /// Used when registering new [`Interceptor`]s in the internal [`BackgroundTasks`] instance.
    const CHANNEL_SIZE: usize = 512;

    /// Retrieves or creates an [`Interceptor`] for the given [`HttpRequestFallback`].
    /// The request may or may not belong to an existing connection (when stealing with an http
    /// filter, connections are created implicitly).
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
                port = request.port,
                connection_id = request.connection_id,
                request_id = request.request_id,
                "Received a new request within a stale port subscription, sending an unsubscribe request or an error response."
            );

            match self.http_gateways.entry(request.connection_id) {
                // This is a new connection, we can just unsubscribe it.
                Entry::Vacant(..) => {
                    message_bus
                        .send(ClientMessage::TcpSteal(
                            LayerTcpSteal::ConnectionUnsubscribe(request.connection_id),
                        ))
                        .await;
                }

                // This is not a new connection, but we don't have any requests in progress.
                // We can still unsubscribe it.
                Entry::Occupied(e) if e.get().is_empty() => {
                    message_bus
                        .send(ClientMessage::TcpSteal(
                            LayerTcpSteal::ConnectionUnsubscribe(request.connection_id),
                        ))
                        .await;
                    e.remove();
                }

                // This is not a new connection, and we have requests in progress.
                // We can only send an error response.
                Entry::Occupied(..) => {
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

    /// Handles all agent messages.
    #[tracing::instrument(level = Level::TRACE, skip(self, message_bus), err)]
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
                let tx: Option<&TaskSender<TcpProxyTask>> = if is_steal {
                    self.steal_tcp_proxies.get(&data.connection_id)
                } else {
                    self.mirror_tcp_proxies.get(&data.connection_id)
                };

                if let Some(tx) = tx {
                    tx.send(data.bytes).await;
                } else {
                    tracing::debug!(
                        connection_id = data.connection_id,
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
                match request {
                    ChunkedRequest::Start(request) => {
                        let (body_tx, body_rx) = mpsc::channel(128);
                        let request =
                            request.map_body(|frames| StreamingBody::new(body_rx, frames));
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
                            return Ok(());
                        };

                        let Some(tx) = gateway.body_tx.as_ref() else {
                            return Ok(());
                        };

                        for frame in frames {
                            if let Err(err) = tx.send(frame).await {
                                tracing::debug!(
                                    frame = ?err.0,
                                    connection_id,
                                    request_id,
                                    "Failed to send an HTTP request body frame to the HttpGateway task, channel is closed"
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
                };
            }

            DaemonTcp::NewConnection(NewTcpConnection {
                connection_id,
                remote_address,
                destination_port,
                source_port,
                local_address,
            }) => {
                let subscription =
                    self.subscriptions
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

                let socket =
                    BoundTcpSocket::bind_specified_or_localhost(subscription.listening_on.ip())
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

    #[tracing::instrument(level = Level::TRACE, skip(self, message_bus))]
    async fn handle_task_update(
        &mut self,
        id: InProxyTask,
        update: TaskUpdate<InProxyTaskMessage, InProxyTaskError>,
        message_bus: &mut MessageBus<Self>,
    ) {
        match (id, update) {
            (InProxyTask::MirrorTcpProxy(connection_id), TaskUpdate::Finished(result)) => {
                match result {
                    Err(TaskError::Error(error)) => {
                        tracing::warn!(connection_id, %error, "MirrorTcpProxy task failed");
                    }
                    Err(TaskError::Panic) => {
                        tracing::error!(connection_id, "MirrorTcpProxy task panicked");
                    }
                    Ok(()) => {}
                };

                self.metadata_store.no_longer_expect(connection_id);

                if self.mirror_tcp_proxies.remove(&connection_id).is_some() {
                    message_bus
                        .send(ClientMessage::Tcp(LayerTcp::ConnectionUnsubscribe(
                            connection_id,
                        )))
                        .await;
                }
            }

            (InProxyTask::MirrorTcpProxy(..), TaskUpdate::Message(..)) => unreachable!(),

            (InProxyTask::StealTcpProxy(connection_id), TaskUpdate::Finished(result)) => {
                match result {
                    Err(TaskError::Error(error)) => {
                        tracing::warn!(connection_id, %error, "StealTcpProxy task failed");
                    }
                    Err(TaskError::Panic) => {
                        tracing::error!(connection_id, "StealTcpProxy task panicked");
                    }
                    Ok(()) => {}
                };

                self.metadata_store.no_longer_expect(connection_id);

                if self.steal_tcp_proxies.remove(&connection_id).is_some() {
                    message_bus
                        .send(ClientMessage::Tcp(LayerTcp::ConnectionUnsubscribe(
                            connection_id,
                        )))
                        .await;
                }
            }

            (
                InProxyTask::StealTcpProxy(connection_id),
                TaskUpdate::Message(InProxyTaskMessage::Tcp(bytes)),
            ) => {
                if self.steal_tcp_proxies.contains_key(&connection_id) {
                    message_bus
                        .send(ClientMessage::TcpSteal(LayerTcpSteal::Data(TcpData {
                            connection_id,
                            bytes,
                        })))
                        .await;
                }
            }

            (InProxyTask::StealTcpProxy(..), TaskUpdate::Message(InProxyTaskMessage::Http(..))) => {
                unreachable!()
            }

            (InProxyTask::HttpGateway(id), TaskUpdate::Finished(result)) => {
                let respond_on_panic = self
                    .http_gateways
                    .get_mut(&id.connection_id)
                    .and_then(|gateways| gateways.remove(&id.request_id))
                    .is_some();

                match result {
                    Ok(()) => {}
                    Err(TaskError::Error(
                        InProxyTaskError::IoError(..) | InProxyTaskError::UpgradeError(..),
                    )) => unreachable!(),
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

            (
                InProxyTask::HttpGateway(id),
                TaskUpdate::Message(InProxyTaskMessage::Http(message)),
            ) => {
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

            (InProxyTask::HttpGateway(..), TaskUpdate::Message(InProxyTaskMessage::Tcp(..))) => {
                unreachable!()
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

                Some((id, update)) = self.tasks.next() => self.handle_task_update(id, update, message_bus).await,
            }
        }
    }
}
