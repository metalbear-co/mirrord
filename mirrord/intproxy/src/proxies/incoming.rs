//! Handles the logic of the `incoming` feature.

use std::{
    collections::{hash_map::Entry, HashMap, VecDeque},
    convert::Infallible,
    io,
    net::SocketAddr,
};

use bound_socket::BoundTcpSocket;
use http::PeekedBody;
use hyper::body::Frame;
use metadata_store::MetadataStore;
use mirrord_intproxy_protocol::{
    ConnMetadataRequest, ConnMetadataResponse, IncomingRequest, IncomingResponse, LayerId,
    MessageId, PortSubscription, ProxyToLayerMessage,
};
use mirrord_protocol::{
    tcp::{
        ChunkedHttpBody, ChunkedHttpError, ChunkedRequest, ChunkedResponse, DaemonTcp,
        HttpResponse, InternalHttpBody, InternalHttpBodyFrame, LayerTcp, LayerTcpSteal,
        NewTcpConnection, TcpData, HTTP_CHUNKED_RESPONSE_VERSION, HTTP_FRAMED_VERSION,
    },
    ClientMessage, ConnectionId, Port, RequestId, ResponseError,
};
use response_reader::HttpResponseReader;
use streaming_body::StreamingBody;
use thiserror::Error;
use tokio::sync::mpsc::{self, Sender};
use tracing::Level;

use self::{
    interceptor::{Interceptor, InterceptorError, MessageOut},
    port_subscription_ext::PortSubscriptionExt,
    subscriptions::SubscriptionsManager,
};
use crate::{
    background_tasks::{
        BackgroundTask, BackgroundTasks, MessageBus, TaskError, TaskSender, TaskUpdate,
    },
    main_tasks::{LayerClosed, LayerForked, ToLayer},
    ProxyMessage,
};

mod bound_socket;
mod http;
mod interceptor;
mod metadata_store;
pub mod port_subscription_ext;
mod response_reader;
mod streaming_body;
mod subscriptions;

/// Identifies an [`HttpResponseReader`].
type ReaderId = (ConnectionId, RequestId);

/// Errors that can occur when handling the `incoming` feature.
#[derive(Error, Debug)]
pub enum IncomingProxyError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("subscribing port failed: {0}")]
    SubscriptionFailed(ResponseError),
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

/// Handle for an [`Interceptor`].
struct InterceptorHandle {
    /// A channel for sending messages to the [`Interceptor`] task.
    tx: TaskSender<Interceptor>,
    /// Port subscription that the intercepted connection belongs to.
    subscription: PortSubscription,
    /// Senders for the bodies of in-progress HTTP requests.
    request_body_txs: HashMap<RequestId, Sender<InternalHttpBodyFrame>>,
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
    /// Determines which version of [`LayerTcpSteal`] we use to send HTTP responses to the agent.
    agent_protocol_version: Option<semver::Version>,

    /// [`TaskSender`]s for active [`Interceptor`]s.
    interceptor_handles: HashMap<ConnectionId, InterceptorHandle>,
    /// For receiving updates from [`Interceptor`]s.
    interceptors: BackgroundTasks<ConnectionId, MessageOut, InterceptorError>,

    /// [TaskSender]s for active [`HttpResponseReader`]s.
    ///
    /// Keep the readers alive.
    readers_txs: HashMap<ReaderId, TaskSender<HttpResponseReader>>,
    /// For reading bodies of user app's HTTP responses.
    readers: BackgroundTasks<ReaderId, LayerTcpSteal, Infallible>,
}

impl IncomingProxy {
    /// Used when registering new [`Interceptor`]s in the internal [`BackgroundTasks`] instance.
    const CHANNEL_SIZE: usize = 512;

    /// Retrieves or creates an [`Interceptor`] for the given [`HttpRequestFallback`].
    /// The request may or may not belong to an existing connection (when stealing with an http
    /// filter, connections are created implicitly).
    #[tracing::instrument(level = Level::TRACE, skip(self, message_bus), err)]
    async fn get_or_create_http_interceptor(
        &mut self,
        connection_id: ConnectionId,
        port: Port,
        message_bus: &MessageBus<Self>,
    ) -> Result<Option<&mut InterceptorHandle>, IncomingProxyError> {
        let interceptor = match self.interceptor_handles.entry(connection_id) {
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

                let interceptor = self.interceptors.register(
                    Interceptor::new(interceptor_socket, subscription.listening_on),
                    connection_id,
                    Self::CHANNEL_SIZE,
                );

                e.insert(InterceptorHandle {
                    tx: interceptor,
                    subscription: subscription.subscription.clone(),
                    request_body_txs: Default::default(),
                })
            }
        };

        Ok(Some(interceptor))
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
                self.readers_txs.retain(|id, _| id.0 != close.connection_id);
                self.interceptor_handles.remove(&close.connection_id);
            }

            DaemonTcp::Data(data) => {
                if let Some(interceptor) = self.interceptor_handles.get(&data.connection_id) {
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
                        .tx
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
                        .tx
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
                            interceptor.request_body_txs.insert(request.request_id, tx);
                            interceptor.tx.send(request).await;
                        }
                    }

                    ChunkedRequest::Body(ChunkedHttpBody {
                        frames,
                        is_last,
                        connection_id,
                        request_id,
                    }) => {
                        let Some(interceptor) = self.interceptor_handles.get_mut(&connection_id)
                        else {
                            return Ok(());
                        };

                        let Entry::Occupied(tx) = interceptor.request_body_txs.entry(request_id)
                        else {
                            return Ok(());
                        };

                        let mut send_err = false;

                        for frame in frames {
                            if let Err(err) = tx.get().send(frame).await {
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
                            tx.remove();
                        }
                    }

                    ChunkedRequest::Error(ChunkedHttpError {
                        connection_id,
                        request_id,
                    }) => {
                        if let Some(interceptor) = self.interceptor_handles.get_mut(&connection_id)
                        {
                            interceptor.request_body_txs.remove(&request_id);
                        };

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

                self.metadata_store.expect(
                    ConnMetadataRequest {
                        listener_address: subscription.listening_on,
                        peer_address: interceptor_socket.local_addr()?,
                    },
                    connection_id,
                    ConnMetadataResponse {
                        remote_source: SocketAddr::new(remote_address, source_port),
                        local_address,
                    },
                );

                let interceptor = self.interceptors.register(
                    Interceptor::new(interceptor_socket, subscription.listening_on),
                    connection_id,
                    Self::CHANNEL_SIZE,
                );

                self.interceptor_handles.insert(
                    connection_id,
                    InterceptorHandle {
                        tx: interceptor,
                        subscription: subscription.subscription.clone(),
                        request_body_txs: Default::default(),
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
        let tail = match response.internal_response.body.tail.take() {
            Some(tail) => tail,

            // All frames are already fetched, we don't have to wait for the body.
            // We can send just one message.
            None => {
                let message = if self.agent_handles_framed_responses() {
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
                    // We can only use legacy `LayerTcpSteal::HttpResponse`,
                    // which drops trailing headers.
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

        let reader_id = (response.connection_id, response.request_id);
        let response_reader = if self.agent_handles_streamed_responses() {
            let response = response.map_body(|body| {
                body.head
                    .into_iter()
                    .map(InternalHttpBodyFrame::from)
                    .collect::<Vec<_>>()
            });
            let connection_id = response.connection_id;
            let request_id = response.request_id;
            let message = ClientMessage::TcpSteal(LayerTcpSteal::HttpResponseChunked(
                ChunkedResponse::Start(response),
            ));
            message_bus.send(message).await;
            HttpResponseReader::Chunked {
                connection_id,
                request_id,
                body: tail,
            }
        } else if self.agent_handles_framed_responses() {
            response.internal_response.body.tail.replace(tail);
            HttpResponseReader::Framed(response)
        } else {
            response.internal_response.body.tail.replace(tail);
            HttpResponseReader::Legacy(response)
        };

        self.readers.register(response_reader, reader_id, 16);
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
                self.agent_protocol_version.replace(version);
            }
        }

        Ok(())
    }

    #[tracing::instrument(level = Level::TRACE, skip(self, message_bus))]
    async fn handle_interceptor_update(
        &mut self,
        connection_id: ConnectionId,
        update: TaskUpdate<MessageOut, InterceptorError>,
        message_bus: &mut MessageBus<Self>,
    ) {
        match update {
            TaskUpdate::Finished(res) => {
                if let Err(error) = res {
                    tracing::warn!(connection_id, %error, "Incoming interceptor failed");
                }

                self.metadata_store.no_longer_expect(connection_id);

                let msg = self
                    .interceptor_handles
                    .get(&connection_id)
                    .map(|interceptor| {
                        interceptor
                            .subscription
                            .wrap_agent_unsubscribe_connection(connection_id)
                    });
                if let Some(msg) = msg {
                    message_bus.send(msg).await;
                }

                self.interceptor_handles.remove(&connection_id);
            }

            TaskUpdate::Message(msg) => {
                let Some(PortSubscription::Steal(_)) = self
                    .interceptor_handles
                    .get(&connection_id)
                    .map(|interceptor| &interceptor.subscription)
                else {
                    return;
                };

                match msg {
                    MessageOut::Raw(bytes) => {
                        let msg = ClientMessage::TcpSteal(LayerTcpSteal::Data(TcpData {
                            connection_id,
                            bytes,
                        }));

                        message_bus.send(msg).await;
                    }

                    MessageOut::Http(response) => {
                        self.handle_http_response(response, message_bus).await;
                    }
                };
            }
        }
    }

    #[tracing::instrument(level = Level::TRACE, skip(self, message_bus))]
    async fn handle_response_reader_update(
        &mut self,
        connection_id: ConnectionId,
        request_id: RequestId,
        update: TaskUpdate<LayerTcpSteal, Infallible>,
        message_bus: &mut MessageBus<Self>,
    ) {
        match update {
            TaskUpdate::Finished(Ok(())) => {
                self.readers_txs.remove(&(connection_id, request_id));
            }

            TaskUpdate::Finished(Err(TaskError::Panic)) => {
                tracing::error!(connection_id, request_id, "HttpResponseReader panicked");

                self.readers_txs.remove(&(connection_id, request_id));
                if let Some(interceptor) = self.interceptor_handles.remove(&connection_id) {
                    message_bus
                        .send(
                            interceptor
                                .subscription
                                .wrap_agent_unsubscribe_connection(connection_id),
                        )
                        .await;
                }
            }

            TaskUpdate::Message(msg) => {
                message_bus.send(ClientMessage::TcpSteal(msg)).await;
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

                Some((id, update)) = self.interceptors.next() => self.handle_interceptor_update(id, update, message_bus).await,

                Some((id, update)) = self.readers.next() => self.handle_response_reader_update(id.0, id.1, update, message_bus).await,
            }
        }
    }
}
