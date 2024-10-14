//! Handles the logic of the `incoming` feature.

use std::{
    collections::{hash_map::Entry, HashMap},
    fmt, io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
};

use bytes::Bytes;
use futures::StreamExt;
use http::RETRY_ON_RESET_ATTEMPTS;
use http_body_util::StreamBody;
use hyper::body::Frame;
use mirrord_intproxy_protocol::{
    ConnMetadataRequest, ConnMetadataResponse, IncomingRequest, IncomingResponse, LayerId,
    MessageId, PortSubscribe, PortSubscription, PortUnsubscribe, ProxyToLayerMessage,
};
use mirrord_protocol::{
    body_chunks::BodyExt,
    tcp::{
        ChunkedHttpBody, ChunkedHttpError, ChunkedRequest, ChunkedResponse, DaemonTcp, HttpRequest,
        HttpRequestFallback, HttpResponse, HttpResponseFallback, InternalHttpBodyFrame,
        InternalHttpRequest, InternalHttpResponse, LayerTcpSteal, NewTcpConnection,
        ReceiverStreamBody, StreamingBody, TcpData,
    },
    ClientMessage, ConnectionId, RequestId, ResponseError,
};
use thiserror::Error;
use tokio::{
    net::TcpSocket,
    sync::mpsc::{self, Sender},
};
use tokio_stream::{wrappers::ReceiverStream, StreamMap, StreamNotifyClose};
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

mod http;
mod interceptor;
pub mod port_subscription_ext;
mod subscriptions;

/// Creates and binds a new [`TcpSocket`].
/// The socket has the same IP version and address as the given `addr`.
///
/// # Exception
///
/// If the given `addr` is unspecified, this function binds to localhost.
#[tracing::instrument(level = Level::TRACE, ret, err)]
fn bind_similar(addr: SocketAddr) -> io::Result<TcpSocket> {
    match addr.ip() {
        IpAddr::V4(Ipv4Addr::UNSPECIFIED) => {
            let socket = TcpSocket::new_v4()?;
            socket.bind(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0))?;
            Ok(socket)
        }
        IpAddr::V6(Ipv6Addr::UNSPECIFIED) => {
            let socket = TcpSocket::new_v6()?;
            socket.bind(SocketAddr::new(Ipv6Addr::LOCALHOST.into(), 0))?;
            Ok(socket)
        }
        addr @ IpAddr::V4(..) => {
            let socket = TcpSocket::new_v4()?;
            socket.bind(SocketAddr::new(addr, 0))?;
            Ok(socket)
        }
        addr @ IpAddr::V6(..) => {
            let socket = TcpSocket::new_v6()?;
            socket.bind(SocketAddr::new(addr, 0))?;
            Ok(socket)
        }
    }
}

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

/// Store for mapping [`Interceptor`] socket addresses to addresses of the original peers.
#[derive(Default)]
struct MetadataStore {
    prepared_responses: HashMap<ConnMetadataRequest, ConnMetadataResponse>,
    expected_requests: HashMap<InterceptorId, ConnMetadataRequest>,
}

impl MetadataStore {
    fn get(&mut self, req: ConnMetadataRequest) -> ConnMetadataResponse {
        self.prepared_responses
            .remove(&req)
            .unwrap_or_else(|| ConnMetadataResponse {
                remote_source: req.peer_address,
                local_address: req.listener_address.ip(),
            })
    }

    fn expect(&mut self, req: ConnMetadataRequest, from: InterceptorId, res: ConnMetadataResponse) {
        self.expected_requests.insert(from, req.clone());
        self.prepared_responses.insert(req, res);
    }

    fn no_longer_expect(&mut self, from: InterceptorId) {
        let Some(req) = self.expected_requests.remove(&from) else {
            return;
        };
        self.prepared_responses.remove(&req);
    }
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
    response_body_rxs: StreamMap<(ConnectionId, RequestId), StreamNotifyClose<ReceiverStreamBody>>,
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
    #[tracing::instrument(level = Level::TRACE, skip(self))]
    fn get_interceptor_for_http_request(
        &mut self,
        request: &HttpRequestFallback,
    ) -> Result<Option<&TaskSender<Interceptor>>, IncomingProxyError> {
        let id: InterceptorId = InterceptorId(request.connection_id());

        let interceptor = match self.interceptors.entry(id) {
            Entry::Occupied(e) => e.into_mut(),

            Entry::Vacant(e) => {
                let Some(subscription) = self.subscriptions.get(request.port()) else {
                    tracing::trace!(
                        "received a new connection for port {} that is no longer mirrored",
                        request.port(),
                    );

                    return Ok(None);
                };

                let interceptor_socket = bind_similar(subscription.listening_on)?;

                let interceptor = self.background_tasks.register(
                    Interceptor::new(
                        interceptor_socket,
                        subscription.listening_on,
                        self.agent_protocol_version.clone(),
                    ),
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
                    tracing::trace!(
                        "received new data for connection {} that is already closed",
                        data.connection_id
                    );
                }
            }
            DaemonTcp::HttpRequest(req) => {
                let req = HttpRequestFallback::Fallback(req);
                let interceptor = self.get_interceptor_for_http_request(&req)?;
                if let Some(interceptor) = interceptor {
                    interceptor.send(req).await;
                }
            }
            DaemonTcp::HttpRequestFramed(req) => {
                let req = HttpRequestFallback::Framed(req);
                let interceptor = self.get_interceptor_for_http_request(&req)?;
                if let Some(interceptor) = interceptor {
                    interceptor.send(req).await;
                }
            }
            DaemonTcp::HttpRequestChunked(req) => {
                match req {
                    ChunkedRequest::Start(req) => {
                        let (tx, rx) = mpsc::channel::<InternalHttpBodyFrame>(128);
                        let http_stream = StreamingBody::new(rx);
                        let http_req = HttpRequest {
                            internal_request: InternalHttpRequest {
                                method: req.internal_request.method,
                                uri: req.internal_request.uri,
                                headers: req.internal_request.headers,
                                version: req.internal_request.version,
                                body: http_stream,
                            },
                            connection_id: req.connection_id,
                            request_id: req.request_id,
                            port: req.port,
                        };
                        let key = (http_req.connection_id, http_req.request_id);

                        self.request_body_txs.insert(key, tx.clone());

                        let http_req = HttpRequestFallback::Streamed {
                            request: http_req,
                            retries: 0,
                        };
                        let interceptor = self.get_interceptor_for_http_request(&http_req)?;
                        if let Some(interceptor) = interceptor {
                            interceptor.send(http_req).await;
                        }

                        for frame in req.internal_request.body {
                            if let Err(err) = tx.send(frame).await {
                                self.request_body_txs.remove(&key);
                                tracing::trace!(?err, "error while sending");
                            }
                        }
                    }
                    ChunkedRequest::Body(body) => {
                        let key = &(body.connection_id, body.request_id);
                        let mut send_err = false;
                        if let Some(tx) = self.request_body_txs.get(key) {
                            for frame in body.frames {
                                if let Err(err) = tx.send(frame).await {
                                    send_err = true;
                                    tracing::trace!(?err, "error while sending");
                                }
                            }
                        }
                        if send_err || body.is_last {
                            self.request_body_txs.remove(key);
                        }
                    }
                    ChunkedRequest::Error(err) => {
                        self.request_body_txs
                            .remove(&(err.connection_id, err.request_id));
                        tracing::trace!(?err, "ChunkedRequest error received");
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
                    tracing::trace!("received a new connection for port {destination_port} that is no longer mirrored");
                    return Ok(());
                };

                let interceptor_socket = bind_similar(subscription.listening_on)?;

                let id = InterceptorId(connection_id);

                self.metadata_store.expect(
                    ConnMetadataRequest {
                        listener_address: subscription.listening_on,
                        peer_address: interceptor_socket.local_addr()?,
                    },
                    id,
                    ConnMetadataResponse {
                        remote_source: SocketAddr::new(remote_address, source_port),
                        local_address,
                    },
                );

                let interceptor = self.background_tasks.register(
                    Interceptor::new(
                        interceptor_socket,
                        subscription.listening_on,
                        self.agent_protocol_version.clone(),
                    ),
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
                        self.handle_agent_message(msg, message_bus).await?;
                    }
                    Some(IncomingProxyMessage::AgentSteal(msg)) => {
                        self.handle_agent_message(msg, message_bus).await?;
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

                        self.metadata_store.no_longer_expect(id);

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
                        let msg = match msg {
                            MessageOut::Raw(bytes) => {
                                ClientMessage::TcpSteal(LayerTcpSteal::Data(TcpData {
                                    connection_id: id.0,
                                    bytes,
                                }))
                            },
                            MessageOut::Http(HttpResponseFallback::Fallback(res)) => {
                                ClientMessage::TcpSteal(LayerTcpSteal::HttpResponse(res))
                            },
                            MessageOut::Http(HttpResponseFallback::Framed(res)) => {
                                ClientMessage::TcpSteal(LayerTcpSteal::HttpResponseFramed(res))
                            },
                            MessageOut::Http(HttpResponseFallback::Streamed(response, request)) => {
                                match self.streamed_http_response(response, request).await {
                                    Some(response) => response,
                                    None => continue,
                                }
                            }
                        };
                        message_bus.send(msg).await;
                    },
                },
            }
        }
    }
}

impl IncomingProxy {
    /// Sends back the streamed http response to the agent.
    ///
    /// If we cannot get the next frame of the streamed body, then we retry the whole
    /// process, by sending the original `request` again through the http `interceptor` to
    /// our hyper handler.
    #[allow(clippy::type_complexity)]
    #[tracing::instrument(level = Level::TRACE, skip(self), ret)]
    async fn streamed_http_response(
        &mut self,
        mut response: HttpResponse<StreamBody<ReceiverStream<Result<Frame<Bytes>, hyper::Error>>>>,
        request: Option<HttpRequestFallback>,
    ) -> Option<ClientMessage> {
        let mut body = vec![];
        let key = (response.connection_id, response.request_id);

        match response
            .internal_response
            .body
            .next_frames(true)
            .await
            .map_err(InterceptorError::from)
        {
            Ok(frames) => {
                frames
                    .frames
                    .into_iter()
                    .map(From::from)
                    .for_each(|frame| body.push(frame));

                self.response_body_rxs
                    .insert(key, StreamNotifyClose::new(response.internal_response.body));

                let internal_response = InternalHttpResponse {
                    status: response.internal_response.status,
                    version: response.internal_response.version,
                    headers: response.internal_response.headers,
                    body,
                };
                let response = ChunkedResponse::Start(HttpResponse {
                    port: response.port,
                    connection_id: response.connection_id,
                    request_id: response.request_id,
                    internal_response,
                });
                Some(ClientMessage::TcpSteal(LayerTcpSteal::HttpResponseChunked(
                    response,
                )))
            }
            // Retry on known errors.
            Err(error @ InterceptorError::Reset) | Err(error @ InterceptorError::ConnectionClosedTooSoon(..)) => {
                tracing::warn!(%error, ?request, "Failed to read first frames of streaming HTTP response");

                let interceptor = self
                    .interceptors
                    .get(&InterceptorId(response.connection_id))?;

                if let Some(HttpRequestFallback::Streamed { request, retries }) = request
                    && retries < RETRY_ON_RESET_ATTEMPTS
                {
                    tracing::trace!(
                        ?request,
                        ?retries,
                        "`RST_STREAM` from hyper, retrying the request."
                    );
                    interceptor
                        .tx
                        .send(HttpRequestFallback::Streamed {
                            request,
                            retries: retries + 1,
                        })
                        .await;
                }

                None
            }
            Err(fail) => {
                tracing::warn!(?fail, "Something went wrong, skipping this response!");
                None
            }
        }
    }
}
