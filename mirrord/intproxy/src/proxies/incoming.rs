//! Handles the logic of the `incoming` feature.

use std::{
    collections::{hash_map::Entry, HashMap},
    fmt,
    net::SocketAddr,
};

use hyper::Version;
use mirrord_config::LayerConfig;
use mirrord_protocol::{
    tcp::{DaemonTcp, HttpRequestFallback, HttpResponseFallback, LayerTcpSteal, TcpData},
    ClientMessage, ConnectionId, Port,
};
use thiserror::Error;

use self::{
    http::{v1::HttpV1Connector, v2::HttpV2Connector},
    http_interceptor::{HttpInterceptor, HttpInterceptorError},
    incoming_mode::{IncomingFlavorError, IncomingMode},
    raw_interceptor::{RawInterceptor, RawInterceptorError},
};
use crate::{
    background_tasks::{BackgroundTask, BackgroundTasks, MessageBus, TaskSender, TaskUpdate},
    protocol::{
        IncomingRequest, LocalMessage, MessageId, PortSubscribe, PortUnsubscribe,
        ProxyToLayerMessage,
    },
    request_queue::{RequestQueue, RequestQueueEmpty},
    ProxyMessage,
};

mod http;
mod http_interceptor;
mod incoming_mode;
mod raw_interceptor;

/// Common type for errors of the [`RawInterceptor`] and the [`HttpInterceptor`].
#[derive(Error, Debug)]
enum InterceptorError {
    #[error("{0}")]
    Raw(#[from] RawInterceptorError),
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
    /// `incoming` feature configuration was invalid.
    #[error("invalid configuration: {0}")]
    ConfigurationError(#[from] IncomingFlavorError),
    /// The agent sent an HTTP request with unsupported [`Version`].
    /// [`Version::HTTP_3`] is currently not supported.
    #[error("{0:?} is not supported")]
    UnsupportedHttpVersion(Version),
}

/// Messages consumed by [`IncomingProxy`] running as a [`BackgroundTask`].
pub enum IncomingProxyMessage {
    LayerRequest(MessageId, IncomingRequest),
    AgentMirror(DaemonTcp),
    AgentSteal(DaemonTcp),
}

/// Handles logic and state of the `incoming` feature.
/// Run as a [`BackgroundTask`] by each [`ProxySession`](crate::session::ProxySession).
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
/// 4. The interceptor connects to the socket specified in the original [`PortSubscribe`] request
///    and sends the address of the remote peer encoded with [`codec`](crate::codec).
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
/// 4. The interceptor connects to the socket specified in the original [`PortSubscribe`] request
///    and sends the address of the remote peer encoded with [`codec`](crate::codec).
/// 5. The proxy passes the requests and the responses between the agent and the [`HttpInterceptor`]
///    task. If the proxy does not operate in the `steal` mode, responses coming from the
///    interceptor are discarded.
/// 6. If the layer closes the connection, the [`HttpInterceptor`] exits and the proxy notifies the
///    agent. If the agent closes the connection, the proxy shuts down the [`HttpInterceptor`].
pub struct IncomingProxy {
    /// Mode this proxy operates in.
    flavor: IncomingMode,
    /// Active subscriptions. Maps ports on the remote target to addresses of layer's listeners.
    subscriptions: HashMap<Port, SocketAddr>,
    /// For [`PortSubscribe`] requests.
    subscribe_reqs: RequestQueue,
    /// [`TaskSender`]s for active [`RawInterceptor`]s.
    txs_raw: HashMap<InterceptorId, TaskSender<Vec<u8>>>,
    /// [`TaskSender`]s for active [`HttpInterceptor`]s.
    txs_http: HashMap<InterceptorId, TaskSender<HttpRequestFallback>>,
    /// For managing both [`RawInterceptor`]s and [`HttpInterceptor`]s.
    background_tasks: BackgroundTasks<InterceptorId, InterceptorMessageOut, InterceptorError>,
}

impl IncomingProxy {
    /// Used when registering new [`RawInterceptor`] and [`HttpInterceptor`] tasks in the
    /// [`BackgroundTasks`] struct.
    const CHANNEL_SIZE: usize = 512;

    /// Creates a new instance based on the provided [`LayerConfig`].
    pub fn new(config: &LayerConfig) -> Result<Self, IncomingProxyError> {
        let flavor = IncomingMode::new(config)?;

        Ok(Self {
            flavor,
            subscriptions: Default::default(),
            subscribe_reqs: Default::default(),
            txs_raw: Default::default(),
            txs_http: Default::default(),
            background_tasks: Default::default(),
        })
    }

    /// Stores subscription request id and sends a corresponding request to the agent.
    /// However, if a subscription for the same port already exists, this method does not make any
    /// request to the agent. Instead, it immediately responds to the layer and starts
    /// redirecting connections to the new listener.
    async fn handle_port_subscribe(
        &mut self,
        message_id: MessageId,
        subscribe: PortSubscribe,
        message_bus: &mut MessageBus<Self>,
    ) {
        if let Some(old_socket) = self
            .subscriptions
            .insert(subscribe.port, subscribe.listening_on)
        {
            // Since this struct identifies listening sockets by their port (#1558), we can only
            // forward the incoming traffic to one socket with that port.
            tracing::info!(
                "Received layer subscription message for port {}, listening on {}, while already listening on {}. Sending all incoming traffic to new socket.",
                subscribe.port,
                subscribe.listening_on,
                old_socket,
            );

            message_bus
                .send(ProxyMessage::ToLayer(LocalMessage {
                    message_id,
                    inner: ProxyToLayerMessage::IncomingSubscribe(Ok(())),
                }))
                .await;

            return;
        }

        let msg = self.flavor.wrap_agent_subscribe(subscribe.port);
        message_bus.send(ProxyMessage::ToAgent(msg)).await;

        self.subscribe_reqs.insert(message_id);
    }

    /// Sends a request to the agent to stop sending incoming connections for the specified port.
    async fn handle_port_unsubscribe(
        &mut self,
        unsubscribe: PortUnsubscribe,
        message_bus: &mut MessageBus<Self>,
    ) {
        if self.subscriptions.remove(&unsubscribe.port).is_some() {
            let msg = self.flavor.wrap_agent_unsubscribe(unsubscribe.port);
            message_bus.send(ProxyMessage::ToAgent(msg)).await;
        }
    }

    /// Retrieves or creates [`HttpInterceptor`] for the given [`HttpRequestFallback`].
    /// The request may or may not belong to an existing connection (unlike [`RawInterceptor`]s,
    /// [`HttpInterceptor`]s are created lazily).
    fn get_or_create_http_interceptor(
        &mut self,
        request: &HttpRequestFallback,
    ) -> Result<Option<&TaskSender<HttpRequestFallback>>, IncomingProxyError> {
        let id: InterceptorId = InterceptorId(request.connection_id());

        let interceptor = match self.txs_http.entry(id) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => {
                let Some(local_destination) = self.subscriptions.get(&request.port()).copied()
                else {
                    tracing::trace!(
                        "received a new http request for port {} that is no longer mirrored",
                        request.port()
                    );
                    return Ok(None);
                };

                let version = request.version();
                let interceptor = match version {
                    hyper::Version::HTTP_2 => self.background_tasks.register(
                        HttpInterceptor::<HttpV2Connector>::new(local_destination),
                        InterceptorId(request.connection_id()),
                        Self::CHANNEL_SIZE,
                    ),

                    version @ hyper::Version::HTTP_3 => {
                        return Err(IncomingProxyError::UnsupportedHttpVersion(version))
                    }

                    _http_v1 => self.background_tasks.register(
                        HttpInterceptor::<HttpV1Connector>::new(local_destination),
                        InterceptorId(request.connection_id()),
                        Self::CHANNEL_SIZE,
                    ),
                };

                e.insert(interceptor)
            }
        };

        Ok(Some(interceptor))
    }

    /// Handles all agent messages.
    async fn handle_agent_message(
        &mut self,
        message: DaemonTcp,
        message_bus: &mut MessageBus<Self>,
    ) -> Result<(), IncomingProxyError> {
        match message {
            DaemonTcp::Close(close) => {
                self.txs_raw.remove(&InterceptorId(close.connection_id));
                self.txs_http.remove(&InterceptorId(close.connection_id));
            }
            DaemonTcp::Data(data) => {
                if let Some(interceptor) = self.txs_raw.get(&InterceptorId(data.connection_id)) {
                    interceptor.send(data.bytes).await;
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
            DaemonTcp::NewConnection(connection) => {
                let Some(local_destination) = self
                    .subscriptions
                    .get(&connection.destination_port)
                    .copied()
                else {
                    tracing::trace!(
                        "received a new connection for port {} that is no longer mirrored",
                        connection.destination_port,
                    );
                    return Ok(());
                };

                let remote_source =
                    SocketAddr::new(connection.remote_address, connection.source_port);

                let id = InterceptorId(connection.connection_id);
                let interceptor = self.background_tasks.register(
                    RawInterceptor::new(remote_source, local_destination),
                    id,
                    Self::CHANNEL_SIZE,
                );
                self.txs_raw.insert(id, interceptor);
            }
            DaemonTcp::SubscribeResult(res) => {
                let message_id = self.subscribe_reqs.get()?;

                message_bus
                    .send(ProxyMessage::ToLayer(LocalMessage {
                        message_id,
                        inner: ProxyToLayerMessage::IncomingSubscribe(res.map(|_| ())),
                    }))
                    .await;
            }
        }

        Ok(())
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
                    Some(IncomingProxyMessage::LayerRequest(id, req)) => match req {
                        IncomingRequest::PortSubscribe(subscribe) => self.handle_port_subscribe(id, subscribe, message_bus).await,
                        IncomingRequest::PortUnsubscribe(unsubscribe) => self.handle_port_unsubscribe(unsubscribe, message_bus).await,
                    },
                    Some(IncomingProxyMessage::AgentMirror(msg)) => {
                        if self.flavor.is_steal() {
                            break Err(IncomingProxyError::ReceivedMirrorMessage(msg));
                        }

                        self.handle_agent_message(msg, message_bus).await?;
                    }
                    Some(IncomingProxyMessage::AgentSteal(msg)) => {
                        if !self.flavor.is_steal() {
                            break Err(IncomingProxyError::ReceivedStealMessage(msg));
                        }

                        self.handle_agent_message(msg, message_bus).await?;
                    }
                },

                Some(task_update) = self.background_tasks.next() => match task_update {
                    (id, TaskUpdate::Finished(res)) => {
                        tracing::trace!("{id} finished: {res:?}");
                        if self.txs_raw.contains_key(&id) || self.txs_http.contains_key(&id) {
                            let msg = self.flavor.wrap_agent_unsubscribe_connection(id.0);
                            message_bus.send(ProxyMessage::ToAgent(msg)).await;
                        }
                    },
                    (id, TaskUpdate::Message(msg)) if self.flavor.is_steal() => match msg {
                        InterceptorMessageOut::Bytes(bytes) => {
                            let msg = ClientMessage::TcpSteal(LayerTcpSteal::Data(TcpData {
                                connection_id: id.0,
                                bytes,
                            }));
                            message_bus.send(ProxyMessage::ToAgent(msg)).await;
                        },
                        InterceptorMessageOut::Http(HttpResponseFallback::Fallback(res)) => {
                            let msg = ClientMessage::TcpSteal(LayerTcpSteal::HttpResponse(res));
                            message_bus.send(ProxyMessage::ToAgent(msg)).await;
                        },
                        InterceptorMessageOut::Http(HttpResponseFallback::Framed(res)) => {
                            let msg = ClientMessage::TcpSteal(LayerTcpSteal::HttpResponseFramed(res));
                            message_bus.send(ProxyMessage::ToAgent(msg)).await;
                        }
                    },
                    _ => {}
                },
            }
        }
    }
}
