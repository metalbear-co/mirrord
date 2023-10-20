//! Handles the logic of the `incoming` feature.

use std::{
    collections::{hash_map::Entry, HashMap},
    fmt, io,
    net::{IpAddr, SocketAddr},
};

use hyper::Version;
use mirrord_protocol::{
    tcp::{DaemonTcp, HttpRequestFallback, HttpResponseFallback, NewTcpConnection},
    ConnectionId, Port,
};
use thiserror::Error;
use tokio::net::TcpSocket;

use self::{
    http_interceptor::{HttpInterceptor, HttpInterceptorError},
    port_subscription_ext::PortSubscriptionExt,
    raw_interceptor::RawInterceptor,
};
use crate::{
    background_tasks::{BackgroundTask, BackgroundTasks, MessageBus, TaskSender, TaskUpdate},
    main_tasks::{LayerClosed, LayerForked, ToLayer},
    protocol::{
        ConnMetadataRequest, ConnMetadataResponse, IncomingRequest, IncomingResponse, LayerId,
        MessageId, PortSubscribe, PortSubscription, PortUnsubscribe, ProxyToLayerMessage,
    },
    remote_resources::RemoteResources,
    request_queue::{RequestQueue, RequestQueueEmpty},
    ProxyMessage,
};

mod http;
mod http_interceptor;
mod port_subscription_ext;
mod raw_interceptor;

/// Common type for errors of the [`RawInterceptor`] and the [`HttpInterceptor`].
#[derive(Error, Debug)]
enum InterceptorError {
    #[error("{0}")]
    Raw(#[from] io::Error),
    #[error("{0}")]
    Http(#[from] HttpInterceptorError),
}

/// Common type for messages produced by the [`RawInterceptor`] and the [`HttpInterceptor`].
pub enum InterceptorMessageOut {
    Bytes(Vec<u8>),
    Http(HttpResponseFallback),
}

/// Id of a single interceptor task. Used to manage interceptor tasks with the [`BackgroundTasks`]
/// struct.
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
    /// The proxy received from the agent a message incompatible with the `steal` feature, but it
    /// operates in the `steal` mode. This should never happen.
    #[error("received TCP mirror message while in steal mode: {0:?}")]
    ReceivedMirrorMessage(DaemonTcp),
    /// The proxy received from the agent a message related to the `steal` feature, but it does not
    /// operate in the `steal` mode. This should never happen.
    #[error("received TCP steal message while in mirror mode: {0:?}")]
    ReceivedStealMessage(DaemonTcp),
    /// The agent sent a response, but the corresponding [`RequestQueue`] was empty.
    /// This should never happen.
    #[error("{0}")]
    RequestQueueEmpty(#[from] RequestQueueEmpty),
    /// The agent sent an HTTP request with unsupported [`Version`].
    /// [`Version::HTTP_3`] is currently not supported.
    #[error("{0:?} is not supported")]
    UnsupportedHttpVersion(Version),
    #[error("{0}")]
    Io(#[from] io::Error),
}

/// Messages consumed by [`IncomingProxy`] running as a [`BackgroundTask`].
pub enum IncomingProxyMessage {
    LayerRequest(MessageId, LayerId, IncomingRequest),
    LayerForked(LayerForked),
    LayerClosed(LayerClosed),
    AgentMirror(DaemonTcp),
    AgentSteal(DaemonTcp),
}

struct InterceptorHandle<I: BackgroundTask> {
    tx: TaskSender<I>,
    subscription: PortSubscription,
}

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
/// Handles two types of communication: raw TCP and HTTP.
///
/// # TCP flow
///
/// 1. Proxy receives a [`PortSubscribe`] request from the layer and sends a corresponding request
///    to the agent.
/// 2. Proxy receives a confirmation from the agent and responds to the layer.
/// 3. Proxy receives [`NewTcpConnection`](mirrord_protocol::tcp::NewTcpConnection) messages from
///    the agent. For each connection, it creates a new [`RawInterceptor`] task.
/// 4. The interceptor connects to the socket specified in the original [`PortSubscribe`] request.
/// 5. The proxy passes the data between the agent and the [`RawInterceptor`] task. If the proxy
///    does not operate in the `steal` mode, data coming from the interceptor is discarded.
/// 6. If the layer closes the connection, the [`RawInterceptor`] exits and the proxy notifies the
///    agent. If the agent closes the connection, the proxy shuts down the [`RawInterceptor`].
///
/// # HTTP flow
///
/// 1. Proxy receives a [`PortSubscribe`] request from the layer and sends a corresponding request
///    to the agent.
/// 2. Proxy receives a confirmation from the agent and responds to the layer.
/// 3. Proxy receives [`HttpRequest`](mirrord_protocol::tcp::HttpRequest)s from the agent. If there
///    is no registered [`HttpInterceptor`] task for the [`ConnectionId`] specified in the request,
///    the proxy creates one.
/// 4. The interceptor connects to the socket specified in the original [`PortSubscribe`] request.
/// 5. The proxy passes the requests and the responses between the agent and the [`HttpInterceptor`]
///    task. If the proxy does not operate in the `steal` mode, responses coming from the
///    interceptor are discarded.
/// 6. If the layer closes the connection, the [`HttpInterceptor`] exits and the proxy notifies the
///    agent. If the agent closes the connection, the proxy shuts down the [`HttpInterceptor`].
#[derive(Default)]
pub struct IncomingProxy {
    /// Remote ports subscribed by layers. Allows tracking across layer forks.
    remote_subscriptions: RemoteResources<Port>,
    /// Mapping from subscribed port on the remote target to layer's [`PortSubscribe`] request.
    subscriptions: HashMap<Port, PortSubscribe>,
    /// For matching agent's responses with layer's [`PortSubscribe`] requests.
    subscribe_reqs: RequestQueue,
    /// [`TaskSender`]s for active [`RawInterceptor`]s.
    interceptors_raw: HashMap<InterceptorId, InterceptorHandle<RawInterceptor>>,
    /// [`TaskSender`]s for active [`HttpInterceptor`]s.
    interceptors_http: HashMap<InterceptorId, InterceptorHandle<HttpInterceptor>>,
    /// For receiving updates from both [`RawInterceptor`]s and [`HttpInterceptor`]s.
    background_tasks: BackgroundTasks<InterceptorId, InterceptorMessageOut, InterceptorError>,
    /// For managing intercepted connections metadata.
    metadata_store: MetadataStore,
}

impl IncomingProxy {
    /// Used when registering new [`RawInterceptor`] and [`HttpInterceptor`] tasks in the
    /// [`BackgroundTasks`] struct.
    const CHANNEL_SIZE: usize = 512;

    /// Stores subscription request id and sends a corresponding request to the agent.
    /// However, if a subscription for the same port already exists, this method does not make any
    /// request to the agent. Instead, it immediately responds to the layer and starts
    /// redirecting connections to the new listener.
    #[tracing::instrument(level = "trace", skip(self, message_bus))]
    async fn handle_port_subscribe(
        &mut self,
        message_id: MessageId,
        layer_id: LayerId,
        subscribe: PortSubscribe,
        message_bus: &mut MessageBus<Self>,
    ) {
        if let Some(old_subscription) = self
            .subscriptions
            .insert(subscribe.subscription.port(), subscribe.clone())
        {
            // Since this struct identifies listening sockets by their port (#1558), we can only
            // forward the incoming traffic to one socket with that port.
            tracing::info!(
                "Received layer subscription message for port {}, listening on {}, while already listening on {}. Sending all incoming traffic to new socket.",
                subscribe.subscription.port(),
                subscribe.listening_on,
                old_subscription.listening_on,
            );

            message_bus
                .send(ToLayer {
                    message_id,
                    layer_id,
                    message: ProxyToLayerMessage::Incoming(IncomingResponse::PortSubscribe(Ok(()))),
                })
                .await;

            return;
        }

        let msg = subscribe.subscription.agent_subscribe();
        message_bus.send(ProxyMessage::ToAgent(msg)).await;

        self.subscribe_reqs.insert(message_id, layer_id);
    }

    /// Sends a request to the agent to stop sending incoming connections for the specified port.
    #[tracing::instrument(level = "trace", skip(self, message_bus))]
    async fn handle_port_unsubscribe(
        &mut self,
        unsubscribe: PortUnsubscribe,
        message_bus: &mut MessageBus<Self>,
    ) {
        if let Some(subscription) = self.subscriptions.remove(&unsubscribe.port) {
            let msg = subscription.subscription.wrap_agent_unsubscribe();
            message_bus.send(ProxyMessage::ToAgent(msg)).await;
        }
    }

    /// Retrieves or creates [`HttpInterceptor`] for the given [`HttpRequestFallback`].
    /// The request may or may not belong to an existing connection (unlike [`RawInterceptor`]s,
    /// [`HttpInterceptor`]s are created lazily).
    #[tracing::instrument(level = "trace", skip(self))]
    fn get_or_create_http_interceptor(
        &mut self,
        request: &HttpRequestFallback,
    ) -> Result<Option<&TaskSender<HttpInterceptor>>, IncomingProxyError> {
        let id: InterceptorId = InterceptorId(request.connection_id());

        let interceptor = match self.interceptors_http.entry(id) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => {
                let Some(subscription) = self.subscriptions.get(&request.port()) else {
                    tracing::trace!(
                        "received a new http request for port {} that is no longer mirrored",
                        request.port()
                    );

                    return Ok(None);
                };

                let version = request.version();
                let interceptor = self.background_tasks.register(
                    HttpInterceptor::new(subscription.listening_on, version),
                    InterceptorId(request.connection_id()),
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
    #[tracing::instrument(level = "trace", skip(self, message_bus))]
    async fn handle_agent_message(
        &mut self,
        message: DaemonTcp,
        message_bus: &mut MessageBus<Self>,
    ) -> Result<(), IncomingProxyError> {
        match message {
            DaemonTcp::Close(close) => {
                self.interceptors_raw
                    .remove(&InterceptorId(close.connection_id));
                self.interceptors_http
                    .remove(&InterceptorId(close.connection_id));
            }
            DaemonTcp::Data(data) => {
                if let Some(interceptor) = self
                    .interceptors_raw
                    .get(&InterceptorId(data.connection_id))
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
                let interceptor = self.get_or_create_http_interceptor(&req)?;
                if let Some(interceptor) = interceptor {
                    interceptor.send(req).await;
                }
            }
            DaemonTcp::HttpRequestFramed(req) => {
                let req = HttpRequestFallback::Framed(req);
                let interceptor = self.get_or_create_http_interceptor(&req)?;
                if let Some(interceptor) = interceptor {
                    interceptor.send(req).await;
                }
            }
            DaemonTcp::NewConnection(NewTcpConnection {
                connection_id,
                remote_address,
                destination_port,
                source_port,
                local_address,
            }) => {
                let Some(subscription) = self.subscriptions.get(&destination_port) else {
                    tracing::trace!("received a new connection for port {destination_port} that is no longer mirrored");
                    return Ok(());
                };

                let interceptor_socket = match subscription.listening_on.ip() {
                    addr @ IpAddr::V4(..) => {
                        let socket = TcpSocket::new_v4()?;
                        socket.bind(SocketAddr::new(addr, 0))?;
                        socket
                    }
                    addr @ IpAddr::V6(..) => {
                        let socket = TcpSocket::new_v6()?;
                        socket.bind(SocketAddr::new(addr, 0))?;
                        socket
                    }
                };

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
                    RawInterceptor::new(subscription.listening_on, interceptor_socket),
                    id,
                    Self::CHANNEL_SIZE,
                );
                self.interceptors_raw.insert(
                    id,
                    InterceptorHandle {
                        tx: interceptor,
                        subscription: subscription.subscription.clone(),
                    },
                );
            }
            DaemonTcp::SubscribeResult(res) => {
                let (message_id, layer_id) = self.subscribe_reqs.get()?;

                message_bus
                    .send(ToLayer {
                        message_id,
                        layer_id,
                        message: ProxyToLayerMessage::Incoming(IncomingResponse::PortSubscribe(
                            res.map(|_| ()),
                        )),
                    })
                    .await;
            }
        }

        Ok(())
    }

    fn handle_layer_fork(&mut self, msg: LayerForked) {
        let LayerForked { child, parent } = msg;
        self.remote_subscriptions.clone_all(child, parent);
    }

    async fn handle_layer_close(&mut self, msg: LayerClosed, message_bus: &mut MessageBus<Self>) {
        let LayerClosed { id } = msg;
        for to_close in self.remote_subscriptions.remove_all(id) {
            let Some(subscription) = self.subscriptions.remove(&to_close) else {
                continue;
            };
            message_bus
                .send(subscription.subscription.wrap_agent_unsubscribe())
                .await;
        }
    }

    fn get_subscription(&self, interceptor_id: InterceptorId) -> Option<&PortSubscription> {
        if let Some(handle) = self.interceptors_raw.get(&interceptor_id) {
            Some(&handle.subscription)
        } else if let Some(handle) = self.interceptors_http.get(&interceptor_id) {
            Some(&handle.subscription)
        } else {
            None
        }
    }
}

impl BackgroundTask for IncomingProxy {
    type Error = IncomingProxyError;
    type MessageIn = IncomingProxyMessage;
    type MessageOut = ProxyMessage;

    async fn run(mut self, message_bus: &mut MessageBus<Self>) -> Result<(), Self::Error> {
        loop {
            tokio::select! {
                msg = message_bus.recv() => match msg {
                    None => {
                        tracing::trace!("message bus closed, exiting");
                        break Ok(());
                    },
                    Some(IncomingProxyMessage::LayerRequest(message_id, layer_id, req)) => match req {
                        IncomingRequest::PortSubscribe(subscribe) => self.handle_port_subscribe(message_id, layer_id, subscribe, message_bus).await,
                        IncomingRequest::PortUnsubscribe(unsubscribe) => self.handle_port_unsubscribe(unsubscribe, message_bus).await,
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
                },

                Some(task_update) = self.background_tasks.next() => match task_update {
                    (id, TaskUpdate::Finished(res)) => {
                        tracing::trace!("{id} finished: {res:?}");

                        self.metadata_store.no_longer_expect(id);

                        let msg = self.get_subscription(id).map(|s| s.wrap_agent_unsubscribe_connection(id.0));
                        if let Some(msg) = msg {
                            message_bus.send(ProxyMessage::ToAgent(msg)).await;
                        }
                    },

                    (id, TaskUpdate::Message(msg)) => {
                        let msg = self.get_subscription(id).and_then(|s| s.wrap_response(msg, id.0));
                        if let Some(msg) = msg {
                            message_bus.send(msg).await;
                        }
                    },
                },
            }
        }
    }
}
