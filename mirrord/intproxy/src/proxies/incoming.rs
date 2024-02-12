//! Handles the logic of the `incoming` feature.

use std::{
    collections::{hash_map::Entry, HashMap},
    fmt, io,
    net::{IpAddr, SocketAddr},
};

use mirrord_intproxy_protocol::{
    ConnMetadataRequest, ConnMetadataResponse, IncomingRequest, IncomingResponse, LayerId,
    MessageId, PortSubscribe, PortSubscription, PortUnsubscribe, ProxyToLayerMessage,
};
use mirrord_protocol::{
    tcp::{DaemonTcp, HttpRequestFallback, NewTcpConnection},
    ConnectionId, ResponseError,
};
use thiserror::Error;
use tokio::net::TcpSocket;

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
mod port_subscription_ext;
mod subscriptions;

/// Creates and binds a new [`TcpSocket`].
/// The socket has the same IP version and address as the given `addr`.
fn bind_similar(addr: SocketAddr) -> io::Result<TcpSocket> {
    match addr.ip() {
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
/// or implicitly ([`HttpRequest`](mirrord_protocol::tcp::HttpRequest)).
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
}

impl IncomingProxy {
    /// Used when registering new [`RawInterceptor`] and [`HttpInterceptor`] tasks in the
    /// [`BackgroundTasks`] struct.
    const CHANNEL_SIZE: usize = 512;

    /// Tries to register the new subscription in the [`SubscriptionsManager`].
    #[tracing::instrument(level = "trace", skip(self, message_bus))]
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

    /// Tries to unregister the subscription from the [`SubscriptionManager`].
    #[tracing::instrument(level = "trace", skip(self, message_bus))]
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
    #[tracing::instrument(level = "trace", skip(self))]
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
    #[tracing::instrument(level = "trace", skip(self, message_bus))]
    async fn handle_agent_message(
        &mut self,
        message: DaemonTcp,
        message_bus: &mut MessageBus<Self>,
    ) -> Result<(), IncomingProxyError> {
        match message {
            DaemonTcp::Close(close) => {
                self.interceptors
                    .remove(&InterceptorId(close.connection_id));
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
                },

                Some(task_update) = self.background_tasks.next() => match task_update {
                    (id, TaskUpdate::Finished(res)) => {
                        tracing::trace!("{id} finished: {res:?}");

                        self.metadata_store.no_longer_expect(id);

                        let msg = self.get_subscription(id).map(|s| s.wrap_agent_unsubscribe_connection(id.0));
                        if let Some(msg) = msg {
                            message_bus.send(msg).await;
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
