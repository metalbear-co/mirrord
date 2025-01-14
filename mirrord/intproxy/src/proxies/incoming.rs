//! Handles the logic of the `incoming` feature.

use std::{
    collections::{hash_map::Entry, HashMap, VecDeque},
    fmt, io,
    net::SocketAddr,
};

use bound_socket::BoundTcpSocket;
use futures::StreamExt;
use http::PeekedBody;
use http_body_util::BodyStream;
use hyper::body::{Frame, Incoming};
use metadata_store::MetadataStore;
use mirrord_intproxy_protocol::{
    ConnMetadataRequest, ConnMetadataResponse, IncomingRequest, IncomingResponse, LayerId,
    MessageId, PortSubscribe, PortSubscription, PortUnsubscribe, ProxyToLayerMessage,
};
use mirrord_protocol::{
    tcp::{
        ChunkedHttpBody, ChunkedHttpError, ChunkedRequest, ChunkedResponse, DaemonTcp,
        HttpResponse, InternalHttpBody, InternalHttpBodyFrame, LayerTcp, LayerTcpSteal,
        NewTcpConnection, StreamingBody, TcpData, HTTP_CHUNKED_RESPONSE_VERSION,
        HTTP_FRAMED_VERSION,
    },
    ClientMessage, ConnectionId, Port, RequestId, ResponseError,
};
use thiserror::Error;
use tokio::sync::mpsc::{self, Sender};
use tokio_stream::{StreamMap, StreamNotifyClose};
use tracing::{debug, Level};

use self::{
    interceptor::{Interceptor, InterceptorError, MessageOut},
    port_subscription_ext::PortSubscriptionExt,
    subscriptions::SubscriptionsManager,
};
use crate::{
    background_tasks::{BackgroundTask, BackgroundTasks, MessageBus, TaskSender, TaskUpdate},
    main_tasks::{LayerClosed, LayerForked, ToLayer},
    ProxyMessage,
};

mod bound_socket;
mod http;
mod interceptor;
mod metadata_store;
pub mod port_subscription_ext;
mod subscriptions;

/// Id of a single [`Interceptor`] task. Used to manage interceptor tasks with the
/// [`BackgroundTasks`] struct.
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub struct InterceptorId(pub ConnectionId);

impl fmt::Display for InterceptorId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "incoming interceptor {}", self.0,)
    }
}

/// Errors that can occur when handling the `incoming` feature.
#[derive(Error, Debug)]
pub enum IncomingProxyError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("subscribing port failed: {0}")]
    SubscriptionFailed(ResponseError),
}

/// Messages consumed by [`IncomingProxy`] running as a [`BackgroundTask`].
pub enum IncomingProxyMessage {
    LayerRequest(MessageId, LayerId, IncomingRequest),
    LayerForked(LayerForked),
    LayerClosed(LayerClosed),
    AgentMirror(DaemonTcp),
    AgentSteal(DaemonTcp),
    /// Agent responded to [`ClientMessage::SwitchProtocolVersion`].
    AgentProtocolVersion(semver::Version),
}

/// Handle for an [`Interceptor`].
struct InterceptorHandle {
    /// A channel for sending messages to the [`Interceptor`] task.
    tx: TaskSender<Interceptor>,
    /// Port subscription that the intercepted connection belongs to.
    subscription: PortSubscription,
}

/// Handles logic and state of the `incoming` feature.
/// Run as a [`BackgroundTask`].
///
/// Handles port subscriptions state of the connected layers. Utilizes multiple background tasks
/// ([`Interceptor`]s) to handle incoming connections. Each connection is managed by a single
/// [`Interceptor`], that establishes a TCP connection with the user application's port and proxies
/// data.
///
/// Incoming connections are created by the agent either explicitly ([`NewTcpConnection`] message)
/// or implicitly ([`HttpRequest`]).
#[derive(Default)]
pub struct IncomingProxy {
    /// Active port subscriptions for all layers.
    subscriptions: SubscriptionsManager,
    /// [`TaskSender`]s for active [`Interceptor`]s.
    interceptors: HashMap<InterceptorId, InterceptorHandle>,
    /// For receiving updates from [`Interceptor`]s.
    background_tasks: BackgroundTasks<InterceptorId, MessageOut, InterceptorError>,
    /// For managing intercepted connections metadata.
    metadata_store: MetadataStore,
    /// For managing streamed [`DaemonTcp::HttpRequestChunked`] request channels.
    request_body_txs: HashMap<(ConnectionId, RequestId), Sender<InternalHttpBodyFrame>>,
    /// For managing streamed [`LayerTcpSteal::HttpResponseChunked`] response streams.
    response_body_rxs:
        StreamMap<(ConnectionId, RequestId), StreamNotifyClose<BodyStream<Incoming>>>,
    /// Version of [`mirrord_protocol`] negotiated with the agent.
    agent_protocol_version: Option<semver::Version>,
}

impl IncomingProxy {
    /// Used when registering new `RawInterceptor` and `HttpInterceptor` tasks in the
    /// [`BackgroundTasks`] struct.
    // TODO: Update outdated documentation. RawInterceptor, HttpInterceptor do not exist
    const CHANNEL_SIZE: usize = 512;

    /// Tries to register the new subscription in the [`SubscriptionsManager`].
    #[tracing::instrument(level = Level::TRACE, skip(self, message_bus))]
    async fn handle_port_subscribe(
        &mut self,
        message_id: MessageId,
        layer_id: LayerId,
        subscribe: PortSubscribe,
        message_bus: &mut MessageBus<Self>,
    ) {
        let msg = self
            .subscriptions
            .layer_subscribed(layer_id, message_id, subscribe);

        if let Some(msg) = msg {
            message_bus.send(msg).await;
        }
    }

    /// Tries to unregister the subscription from the [`SubscriptionsManager`].
    #[tracing::instrument(level = Level::TRACE, skip(self, message_bus))]
    async fn handle_port_unsubscribe(
        &mut self,
        layer_id: LayerId,
        request: PortUnsubscribe,
        message_bus: &mut MessageBus<Self>,
    ) {
        let msg = self.subscriptions.layer_unsubscribed(layer_id, request);

        if let Some(msg) = msg {
            message_bus.send(msg).await;
        }
    }

    /// Retrieves or creates an [`Interceptor`] for the given [`HttpRequestFallback`].
    /// The request may or may not belong to an existing connection (when stealing with an http
    /// filter, connections are created implicitly).
    #[tracing::instrument(level = Level::TRACE, skip(self, message_bus))]
    async fn get_or_create_http_interceptor(
        &mut self,
        connection_id: ConnectionId,
        port: Port,
        message_bus: &MessageBus<Self>,
    ) -> Result<Option<&TaskSender<Interceptor>>, IncomingProxyError> {
        let id: InterceptorId = InterceptorId(connection_id);

        let interceptor = match self.interceptors.entry(id) {
            Entry::Occupied(e) => e.into_mut(),

            Entry::Vacant(e) => {
                let Some(subscription) = self.subscriptions.get(port) else {
                    tracing::debug!(
                        port,
                        connection_id,
                        "Received a new connection for a port that is no longer subscribed, \
                        sending an unsubscribe request.",
                    );

                    message_bus
                        .send(ClientMessage::TcpSteal(
                            LayerTcpSteal::ConnectionUnsubscribe(connection_id),
                        ))
                        .await;

                    return Ok(None);
                };

                let interceptor_socket =
                    BoundTcpSocket::bind_specified_or_localhost(subscription.listening_on.ip())?;

                let interceptor = self.background_tasks.register(
                    Interceptor::new(interceptor_socket, subscription.listening_on),
                    id,
                    Self::CHANNEL_SIZE,
                );

                e.insert(InterceptorHandle {
                    tx: interceptor,
                    subscription: subscription.subscription.clone(),
                })
            }
        };

        Ok(Some(&interceptor.tx))
    }

    /// Handles all agent messages.
    #[tracing::instrument(level = Level::TRACE, skip(self, message_bus))]
    async fn handle_agent_message(
        &mut self,
        message: DaemonTcp,
        is_steal: bool,
        message_bus: &mut MessageBus<Self>,
    ) -> Result<(), IncomingProxyError> {
        match message {
            DaemonTcp::Close(close) => {
                self.interceptors
                    .remove(&InterceptorId(close.connection_id));
                self.request_body_txs
                    .retain(|(connection_id, _), _| *connection_id != close.connection_id);
                let keys: Vec<(ConnectionId, RequestId)> = self
                    .response_body_rxs
                    .keys()
                    .filter(|key| key.0 == close.connection_id)
                    .cloned()
                    .collect();
                for key in keys.iter() {
                    self.response_body_rxs.remove(key);
                }
            }

            DaemonTcp::Data(data) => {
                if let Some(interceptor) = self.interceptors.get(&InterceptorId(data.connection_id))
                {
                    interceptor.tx.send(data.bytes).await;
                } else {
                    tracing::debug!(
                        connection_id = data.connection_id,
                        "Received new data for a connection that is already closed",
                    );
                }
            }

            DaemonTcp::HttpRequest(request) => {
                let interceptor = self
                    .get_or_create_http_interceptor(
                        request.connection_id,
                        request.port,
                        message_bus,
                    )
                    .await?;

                if let Some(interceptor) = interceptor {
                    interceptor
                        .send(request.map_body(StreamingBody::from))
                        .await;
                }
            }

            DaemonTcp::HttpRequestFramed(request) => {
                let interceptor = self
                    .get_or_create_http_interceptor(
                        request.connection_id,
                        request.port,
                        message_bus,
                    )
                    .await?;

                if let Some(interceptor) = interceptor {
                    interceptor
                        .send(request.map_body(StreamingBody::from))
                        .await;
                }
            }

            DaemonTcp::HttpRequestChunked(request) => {
                match request {
                    ChunkedRequest::Start(request) => {
                        let interceptor = self
                            .get_or_create_http_interceptor(
                                request.connection_id,
                                request.port,
                                message_bus,
                            )
                            .await?;

                        if let Some(interceptor) = interceptor {
                            let (tx, rx) = mpsc::channel::<InternalHttpBodyFrame>(128);
                            let request = request.map_body(|frames| StreamingBody::new(rx, frames));
                            let key = (request.connection_id, request.request_id);
                            interceptor.send(request).await;
                            self.request_body_txs.insert(key, tx);
                        }
                    }

                    ChunkedRequest::Body(ChunkedHttpBody {
                        frames,
                        is_last,
                        connection_id,
                        request_id,
                    }) => {
                        if let Some(tx) = self.request_body_txs.get(&(connection_id, request_id)) {
                            let mut send_err = false;

                            for frame in frames {
                                if let Err(err) = tx.send(frame).await {
                                    send_err = true;
                                    tracing::debug!(
                                        frame = ?err.0,
                                        connection_id,
                                        request_id,
                                        "Failed to send an HTTP request body frame to the interceptor, channel is closed"
                                    );
                                    break;
                                }
                            }

                            if send_err || is_last {
                                self.request_body_txs.remove(&(connection_id, request_id));
                            }
                        }
                    }

                    ChunkedRequest::Error(ChunkedHttpError {
                        connection_id,
                        request_id,
                    }) => {
                        self.request_body_txs.remove(&(connection_id, request_id));
                        tracing::debug!(
                            connection_id,
                            request_id,
                            "Received an error in an HTTP request body",
                        );
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
                let Some(subscription) = self.subscriptions.get(destination_port) else {
                    tracing::debug!(
                        port = destination_port,
                        connection_id,
                        "Received a new connection for a port that is no longer subscribed, \
                        sending an unsubscribe request.",
                    );

                    let message = if is_steal {
                        ClientMessage::Tcp(LayerTcp::ConnectionUnsubscribe(connection_id))
                    } else {
                        ClientMessage::TcpSteal(LayerTcpSteal::ConnectionUnsubscribe(connection_id))
                    };
                    message_bus.send(message).await;

                    return Ok(());
                };

                let interceptor_socket =
                    BoundTcpSocket::bind_specified_or_localhost(subscription.listening_on.ip())?;
                let id = InterceptorId(connection_id);

                self.metadata_store.expect(
                    ConnMetadataRequest {
                        listener_address: subscription.listening_on,
                        peer_address: interceptor_socket.local_addr()?,
                    },
                    id.0,
                    ConnMetadataResponse {
                        remote_source: SocketAddr::new(remote_address, source_port),
                        local_address,
                    },
                );

                let interceptor = self.background_tasks.register(
                    Interceptor::new(interceptor_socket, subscription.listening_on),
                    id,
                    Self::CHANNEL_SIZE,
                );

                self.interceptors.insert(
                    id,
                    InterceptorHandle {
                        tx: interceptor,
                        subscription: subscription.subscription.clone(),
                    },
                );
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

    fn handle_layer_fork(&mut self, msg: LayerForked) {
        let LayerForked { child, parent } = msg;
        self.subscriptions.layer_forked(parent, child);
    }

    async fn handle_layer_close(&mut self, msg: LayerClosed, message_bus: &MessageBus<Self>) {
        let msgs = self.subscriptions.layer_closed(msg.id);

        for msg in msgs {
            message_bus.send(msg).await;
        }
    }

    fn get_subscription(&self, interceptor_id: InterceptorId) -> Option<&PortSubscription> {
        self.interceptors
            .get(&interceptor_id)
            .map(|handle| &handle.subscription)
    }

    /// Handles an HTTP response coming from one of the interceptors.
    ///
    /// If all response frames are already available, sends the response in a single message.
    /// Otherwise, starts a response reader to handle the response.
    #[tracing::instrument(level = Level::TRACE, skip(self, message_bus))]
    async fn handle_http_response(
        &mut self,
        mut response: HttpResponse<PeekedBody>,
        message_bus: &mut MessageBus<Self>,
    ) {
        let _tail = match response.internal_response.body.tail.take() {
            Some(tail) => tail,

            // All frames are already fetched, we don't have to stream the body to the agent.
            None => {
                let message = if self.agent_handles_framed_responses() {
                    // We can send just one message to the agent.
                    let response = response.map_body(|body| {
                        InternalHttpBody(
                            body.head
                                .into_iter()
                                .map(InternalHttpBodyFrame::from)
                                .collect::<VecDeque<_>>(),
                        )
                    });
                    LayerTcpSteal::HttpResponseFramed(response)
                } else {
                    // Agent does not support `LayerTcpSteal::HttpResponseFramed`.
                    // We can only use legacy `LayerTcpSteal::HttpResponse`, which drops trailing
                    // headers.
                    let connection_id = response.connection_id;
                    let request_id = response.request_id;
                    let response = response.map_body(|body| {
                        let mut new_body = Vec::with_capacity(body.head.iter().filter_map(Frame::data_ref).map(|data| data.len()).sum());
                        body.head.into_iter().for_each(|frame| match frame.into_data() {
                            Ok(data) => new_body.extend(data),
                            Err(frame) => {
                                if let Some(headers) = frame.trailers_ref() {
                                    tracing::warn!(
                                        connection_id,
                                        request_id,
                                        agent_protocol_version = ?self.agent_protocol_version,
                                        ?headers,
                                        "Agent uses an outdated version of mirrord protocol, \
                                        we can't send trailing headers from the local application's HTTP response."
                                    )
                                }
                            }
                        });
                        new_body
                    });
                    LayerTcpSteal::HttpResponse(response)
                };

                message_bus.send(ClientMessage::TcpSteal(message)).await;

                return;
            }
        };

        if self.agent_handles_streamed_responses() {
            let response = response.map_body(|body| {
                body.head
                    .into_iter()
                    .map(InternalHttpBodyFrame::from)
                    .collect::<Vec<_>>()
            });
            let message = ClientMessage::TcpSteal(LayerTcpSteal::HttpResponseChunked(
                ChunkedResponse::Start(response),
            ));
            message_bus.send(message).await;
            todo!("start response reader")
        } else if self.agent_handles_framed_responses() {
            todo!("start response reader")
        } else {
            todo!("start response reader")
        }
    }

    fn agent_handles_framed_responses(&self) -> bool {
        self.agent_protocol_version
            .as_ref()
            .map(|version| HTTP_FRAMED_VERSION.matches(version))
            .unwrap_or_default()
    }

    fn agent_handles_streamed_responses(&self) -> bool {
        self.agent_protocol_version
            .as_ref()
            .map(|version| HTTP_CHUNKED_RESPONSE_VERSION.matches(version))
            .unwrap_or_default()
    }
}

impl BackgroundTask for IncomingProxy {
    type Error = IncomingProxyError;
    type MessageIn = IncomingProxyMessage;
    type MessageOut = ProxyMessage;

    #[tracing::instrument(level = Level::TRACE, skip_all, err)]
    async fn run(mut self, message_bus: &mut MessageBus<Self>) -> Result<(), Self::Error> {
        loop {
            tokio::select! {
                Some(((connection_id, request_id), stream_item)) = self.response_body_rxs.next() => match stream_item {
                    Some(Ok(frame)) => {
                        let int_frame = InternalHttpBodyFrame::from(frame);
                        let res = ChunkedResponse::Body(ChunkedHttpBody {
                            frames: vec![int_frame],
                            is_last: false,
                            connection_id,
                            request_id,
                        });
                        message_bus
                            .send(ClientMessage::TcpSteal(LayerTcpSteal::HttpResponseChunked(
                                res,
                            )))
                            .await;
                    },
                    Some(Err(error)) => {
                        debug!(%error, "Error while reading streamed response body");
                        let res = ChunkedResponse::Error(ChunkedHttpError {connection_id, request_id});
                        message_bus
                            .send(ClientMessage::TcpSteal(LayerTcpSteal::HttpResponseChunked(
                                res,
                            )))
                            .await;
                        self.response_body_rxs.remove(&(connection_id, request_id));
                    },
                    None => {
                        let res = ChunkedResponse::Body(ChunkedHttpBody {
                            frames: vec![],
                            is_last: true,
                            connection_id,
                            request_id,
                        });
                        message_bus
                            .send(ClientMessage::TcpSteal(LayerTcpSteal::HttpResponseChunked(
                                res,
                            )))
                            .await;
                        self.response_body_rxs.remove(&(connection_id, request_id));
                    }
                },

                msg = message_bus.recv() => match msg {
                    None => {
                        tracing::trace!("message bus closed, exiting");
                        break Ok(());
                    },
                    Some(IncomingProxyMessage::LayerRequest(message_id, layer_id, req)) => match req {
                        IncomingRequest::PortSubscribe(subscribe) => self.handle_port_subscribe(message_id, layer_id, subscribe, message_bus).await,
                        IncomingRequest::PortUnsubscribe(unsubscribe) => self.handle_port_unsubscribe(layer_id, unsubscribe, message_bus).await,
                        IncomingRequest::ConnMetadata(req) => {
                            let res = self.metadata_store.get(req);
                            message_bus.send(ToLayer { message_id, layer_id, message: ProxyToLayerMessage::Incoming(IncomingResponse::ConnMetadata(res))  }).await;
                        }
                    },
                    Some(IncomingProxyMessage::AgentMirror(msg)) => {
                        self.handle_agent_message(msg, false, message_bus).await?;
                    }
                    Some(IncomingProxyMessage::AgentSteal(msg)) => {
                        self.handle_agent_message(msg, true, message_bus).await?;
                    }
                    Some(IncomingProxyMessage::LayerClosed(msg)) => self.handle_layer_close(msg, message_bus).await,
                    Some(IncomingProxyMessage::LayerForked(msg)) => self.handle_layer_fork(msg),
                    Some(IncomingProxyMessage::AgentProtocolVersion(version)) => {
                        self.agent_protocol_version.replace(version);
                    }
                },

                Some(task_update) = self.background_tasks.next() => match task_update {
                    (id, TaskUpdate::Finished(res)) => {
                        tracing::trace!("{id} finished: {res:?}");

                        self.metadata_store.no_longer_expect(id.0);

                        let msg = self.get_subscription(id).map(|s| s.wrap_agent_unsubscribe_connection(id.0));
                        if let Some(msg) = msg {
                            message_bus.send(msg).await;
                        }

                        self.request_body_txs.retain(|(connection_id, _), _| *connection_id != id.0);
                    },

                    (id, TaskUpdate::Message(msg)) => {
                        let Some(PortSubscription::Steal(_)) = self.get_subscription(id) else {
                            continue;
                        };

                        match msg {
                            MessageOut::Raw(bytes) => {
                                let msg = ClientMessage::TcpSteal(LayerTcpSteal::Data(TcpData {
                                    connection_id: id.0,
                                    bytes,
                                }));

                                message_bus.send(msg).await;
                            },

                            MessageOut::Http(response) => {
                                self.handle_http_response(response, message_bus).await;
                            }
                        };
                    },
                },
            }
        }
    }
}
