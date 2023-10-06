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
    flavor::{Flavor, IncomingFlavorError},
    http::{v1::HttpV1, v2::HttpV2},
    http_interceptor::{HttpInterceptor, HttpInterceptorError},
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

mod flavor;
mod http;
mod http_interceptor;
mod raw_interceptor;

#[derive(Error, Debug)]
enum InterceptorError {
    #[error("{0}")]
    Raw(#[from] RawInterceptorError),
    #[error("{0}")]
    Http(#[from] HttpInterceptorError),
}

pub enum InterceptorMessageOut {
    Bytes(Vec<u8>),
    Http(HttpResponseFallback),
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub struct InterceptorId(pub ConnectionId);

impl fmt::Display for InterceptorId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "incoming interceptor {}", self.0,)
    }
}

#[derive(Error, Debug)]
pub enum IncomingProxyError {
    #[error("received TCP mirror message while in steal mode: {0:?}")]
    ReceivedMirrorMessage(DaemonTcp),
    #[error("received TCP steal message while in mirror mode: {0:?}")]
    ReceivedStealMessage(DaemonTcp),
    #[error("{0}")]
    RequestQueueEmpty(#[from] RequestQueueEmpty),
    #[error("invalid configuration: {0}")]
    ConfigurationError(#[from] IncomingFlavorError),
    #[error("{0:?} is not supported")]
    UnsupportedHttpVersion(Version),
}

pub enum IncomingProxyMessage {
    LayerRequest(MessageId, IncomingRequest),
    AgentMirror(DaemonTcp),
    AgentSteal(DaemonTcp),
}

pub struct IncomingProxy {
    flavor: Flavor,
    subscriptions: HashMap<Port, SocketAddr>,
    subscribe_reqs: RequestQueue,
    txs_raw: HashMap<InterceptorId, TaskSender<Vec<u8>>>,
    txs_http: HashMap<InterceptorId, TaskSender<HttpRequestFallback>>,
    background_tasks: BackgroundTasks<InterceptorId, InterceptorMessageOut, InterceptorError>,
}

impl IncomingProxy {
    const CHANNEL_SIZE: usize = 512;

    pub fn new(config: &LayerConfig) -> Result<Self, IncomingProxyError> {
        let flavor = Flavor::new(config)?;

        Ok(Self {
            flavor,
            subscriptions: Default::default(),
            subscribe_reqs: Default::default(),
            txs_raw: Default::default(),
            txs_http: Default::default(),
            background_tasks: Default::default(),
        })
    }

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
                    return Ok(None);
                };

                let version = request.version();
                let interceptor = match version {
                    hyper::Version::HTTP_2 => self.background_tasks.register(
                        HttpInterceptor::<HttpV2>::new(local_destination),
                        InterceptorId(request.connection_id()),
                        Self::CHANNEL_SIZE,
                    ),

                    version @ hyper::Version::HTTP_3 => {
                        return Err(IncomingProxyError::UnsupportedHttpVersion(version))
                    }

                    _http_v1 => self.background_tasks.register(
                        HttpInterceptor::<HttpV1>::new(local_destination),
                        InterceptorId(request.connection_id()),
                        Self::CHANNEL_SIZE,
                    ),
                };

                e.insert(interceptor)
            }
        };

        Ok(Some(interceptor))
    }

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
                    None => break Ok(()),
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

                task_update = self.background_tasks.next() => match task_update {
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
