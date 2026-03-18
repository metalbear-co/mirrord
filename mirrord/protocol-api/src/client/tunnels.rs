use std::{
    collections::{HashMap, hash_map::Entry},
    fmt,
    num::NonZeroUsize,
    ops::{Deref, DerefMut, Not},
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use bytes::Bytes;
use futures::{SinkExt, Stream, StreamExt};
use hyper::StatusCode;
use mirrord_protocol::{
    ClientMessage,
    tcp::{
        ChunkedRequestBodyV1, ChunkedRequestErrorV1, ChunkedResponse, HttpResponse,
        InternalHttpBodyNew, InternalHttpRequest, InternalHttpResponse, LayerTcpSteal,
    },
};
use tokio::{sync::oneshot, time};

pub use crate::client::tunnels::id::{TunnelId, TunnelType};
use crate::{
    client::{
        error::{TaskError, TaskResult},
        outbox::OutBox,
        tunnels::streams::{
            AbortOnDrop, InternalResponseBody, InternalStreams, ResponseMode, StreamEvent,
        },
    },
    fifo::{Fifo, FifoClosedError, FifoSink},
    shrinkable::Shrinkable,
    traffic::{BodyError, ReceivedBody, TunneledData, TunneledRequest},
};

mod id;
mod streams;
#[cfg(test)]
mod test;

/// State of **all** traffic tunnels maintained by a [`mirrord_protocol`] client.
///
/// Should be polled with [`Stream::poll_next`] for outbound [`ClientMessage`]s.
#[derive(Debug)]
pub struct TrafficTunnels {
    /// This map stores only half of the state of each traffic tunnel.
    /// The other half lives in [`Self::streams`] as concurrently polled streams.
    tunnels: HashMap<TunnelId, Tunnel>,
    /// Stores internal event streams, which account for half of the state of each traffic tunnel.
    /// The other half lives in [`Self::tunnels`].
    ///
    /// # Important
    ///
    /// [`AbortOnDrop`] returned from [`InternalStreams`] methods is used throughout this struct.
    /// Correctnes of the logic depends on the hard guarantee that after [`AbortOnDrop`] is
    /// dropped, the related stream immediately stops yielding events.
    ///
    /// Do not introduce any kind of background polling or event buffering here.
    streams: InternalStreams,
    /// Capacity for new connections.
    ///
    /// See [`Self::new`] doc for more info.
    connection_capacity: NonZeroUsize,
    /// How long we can block when pushing tunneled data to the client.
    block_timeout: Duration,
    /// Detected from [`mirrord_protocol`] version of our connection with the server.
    response_mode: ResponseMode,
}

impl TrafficTunnels {
    /// Creates a new instance.
    ///
    /// # Params
    ///
    /// * `connection_capacity` - used when creating all [`Fifo`]s that power the tunnels. Note that
    ///   a single connection tunnel consists of multiple [`Fifo`]s. Each one is created with an
    ///   independent capacity limit set to this value.
    /// * `block_timeout` - how long can wait when pushing tunneled data to the client. Exceeding
    ///   this limit forcefully closes the tunnel.
    /// * `protocol_version` - [`mirrord_protocol`] version of our connection with the server.
    ///   Determines how we send HTTP responses to the server.
    pub fn new(
        connection_capacity: NonZeroUsize,
        block_timeout: Duration,
        protocol_version: &semver::Version,
    ) -> Self {
        Self {
            tunnels: Default::default(),
            streams: Default::default(),
            connection_capacity,
            block_timeout,
            response_mode: ResponseMode::most_recent(protocol_version),
        }
    }

    /// Creates a new data tunnel, pushing its streams to [`InternalStreams`].
    fn create_tunnel(
        streams: &mut InternalStreams,
        id: TunnelId,
        capacity: NonZeroUsize,
    ) -> (DataTunnel, TunneledData) {
        let inbound = Fifo::with_capacity(capacity);
        let abort_inbound_watch = streams.push_inbound_data_closed(id, inbound.closed);

        let outbound = Fifo::with_capacity(capacity);
        let abort_outbound_read = id.0.has_outbound().then(|| {
            // If this tunnel has outbound data, the `Fifo` will remain open,
            // as the `FifoStream` will live in `InternalStreams`.
            // Otherwise, `FifoStream` will be dropped,
            // and we will return a closed `Fifo`.
            streams.push_outbound_data(id, outbound.stream)
        });

        (
            DataTunnel {
                inbound: Some(WatchedSink {
                    sink: inbound.sink,
                    _guard: abort_inbound_watch,
                }),
                outbound: abort_outbound_read,
            },
            TunneledData {
                sink: outbound.sink,
                sink_closed: outbound.closed,
                stream: inbound.stream,
            },
        )
    }

    /// Initializes state for a new connection from the [`mirrord_protocol`] server.
    pub fn server_connection(&mut self, id: TunnelId) -> TaskResult<TunneledData> {
        let Entry::Vacant(e) = self.tunnels.entry(id) else {
            return Err(TaskError::ProtocolViolation(
                format!("sent connection with duplicate id {id:?}").into(),
            ));
        };
        let (tunnel, data) = Self::create_tunnel(&mut self.streams, id, self.connection_capacity);
        e.insert(Tunnel::Raw(tunnel));
        Ok(data)
    }

    /// Initializes state for a new HTTP request from the [`mirrord_protocol`] server.
    pub fn server_request(
        &mut self,
        id: TunnelId,
        port: u16,
        request: InternalHttpRequest<InternalHttpBodyNew>,
    ) -> TaskResult<TunneledRequest> {
        let Entry::Vacant(e) = self.tunnels.entry(id) else {
            return Err(TaskError::ProtocolViolation(
                format!("sent request with duplicate id {id:?}",).into(),
            ));
        };

        let request: hyper::Request<InternalHttpBodyNew> = request.into();

        let (body_sink, body_stream) = request
            .body()
            .is_last
            .not()
            .then(|| {
                let fifo = Fifo::with_capacity(self.connection_capacity);
                let guard = self.streams.push_inbound_body_closed(id, fifo.closed);
                (
                    WatchedSink {
                        sink: fifo.sink,
                        _guard: guard,
                    },
                    fifo.stream,
                )
            })
            .unzip();
        let request = request.map(|body| ReceivedBody {
            head: body.frames.into(),
            tail: body_stream,
        });

        let (upgrade_tx, upgrade_rx) = oneshot::channel();
        let (response_tx, response_rx) = oneshot::channel();
        let response = id.0.has_outbound().then(|| {
            // If this tunnel has outbound data, `response_rx` will remain open,
            // as it will live in `InternalStreams`.
            // Otherwise, `response_rx` will be dropped,
            // and we will return a closed channel.
            self.streams
                .push_response(id, response_rx, self.response_mode, request.version())
        });

        e.insert_entry(Tunnel::Request {
            port,
            body_sink,
            response,
            upgrade: UpgradeTunnel::Pending(upgrade_tx),
        });

        Ok(TunneledRequest {
            request: Box::new(request),
            response_tx,
            upgrade_rx,
        })
    }

    /// Consumes data sent to a connection from the [`mirrord_protocol`] server.
    pub async fn server_data(&mut self, id: TunnelId, data: Bytes) -> OutBox {
        let Entry::Occupied(mut e) = self.tunnels.entry(id) else {
            return Default::default();
        };

        let result = match e.get_mut() {
            Tunnel::Raw(tunnel) => {
                time::timeout(self.block_timeout, tunnel.server_data(data)).await
            }
            Tunnel::Request { upgrade, .. } => match upgrade {
                UpgradeTunnel::Active(tunnel) => {
                    time::timeout(self.block_timeout, tunnel.server_data(data)).await
                }
                UpgradeTunnel::Pending(..) => {
                    let (mut tunnel, tunneled_data) =
                        Self::create_tunnel(&mut self.streams, id, self.connection_capacity);
                    tunnel
                        .server_data(data)
                        .await
                        .expect("this cannot fail nor block - channel is open and empty");
                    let UpgradeTunnel::Pending(tx) =
                        std::mem::replace(upgrade, UpgradeTunnel::Active(tunnel))
                    else {
                        unreachable!("was checked in this match arm");
                    };
                    tx.send(tunneled_data).ok();
                    Ok(Ok(()))
                }
                UpgradeTunnel::NoUpgrade => Ok(Err(FifoClosedError)),
            },
        };

        match result {
            Ok(Ok(())) => return Default::default(),
            Ok(Err(FifoClosedError)) => match e.get() {
                // If this is a raw tunnel, write after close is an error.
                Tunnel::Raw(..) => {}
                // If this is a request tunnel, it's much nicer to wait for the response first.
                // The response might tell the original HTTP client *why* we're not reading upgrade
                // data (e.g. early 404).
                Tunnel::Request {
                    response: Some(..), ..
                } => return Default::default(),
                Tunnel::Request { response: None, .. } => {}
            },
            Err(_elapsed) => {
                tracing::warn!(
                    tunnel_id = ?id,
                    tunnel = ?e.get(),
                    timeout = ?self.block_timeout,
                    "Failed to send remote data through a local tunnel \
                    within the configured timeout. \
                    Closing the tunnel.",
                );
            }
        }

        e.remove();
        self.tunnels.smart_shrink();
        id.close_message().into()
    }

    /// Consumes a connection shutdown from the [`mirrord_protocol`] server.
    pub fn server_shutdown(&mut self, id: TunnelId) -> OutBox {
        let Entry::Occupied(mut e) = self.tunnels.entry(id) else {
            return Default::default();
        };

        match e.get_mut() {
            Tunnel::Raw(tunnel) => tunnel.inbound = None,
            Tunnel::Request { upgrade, .. } => match upgrade {
                UpgradeTunnel::Active(tunnel) => tunnel.inbound = None,
                UpgradeTunnel::Pending(..) => {
                    let (mut tunnel, tunneled_data) =
                        Self::create_tunnel(&mut self.streams, id, self.connection_capacity);
                    tunnel.inbound = None;
                    let UpgradeTunnel::Pending(tx) =
                        std::mem::replace(upgrade, UpgradeTunnel::Active(tunnel))
                    else {
                        unreachable!("was checked in this match arm");
                    };
                    tx.send(tunneled_data).ok();
                }
                UpgradeTunnel::NoUpgrade => {}
            },
        }

        if e.get().is_dead() {
            e.remove();
            self.tunnels.entry(id);
            id.close_message().into()
        } else {
            Default::default()
        }
    }

    /// Consumes a connection close from the [`mirrord_protocol`] server.
    pub fn server_close(&mut self, id: TunnelId) {
        if self.tunnels.remove(&id).is_some() {
            self.tunnels.smart_shrink();
        }
    }

    /// Consumes an HTTP request body chunk from the [`mirrord_protocol`] server.
    pub async fn server_request_body(
        &mut self,
        id: TunnelId,
        body: InternalHttpBodyNew,
    ) -> TaskResult<OutBox> {
        let Entry::Occupied(mut e) = self.tunnels.entry(id) else {
            return Ok(Default::default());
        };
        let Tunnel::Request {
            body_sink,
            response,
            ..
        } = e.get_mut()
        else {
            return Err(TaskError::ProtocolViolation(
                format!("sent request body to a raw connection ({id:?})").into(),
            ));
        };

        let is_last = body.is_last;

        let result = match body_sink {
            Some(sink) => time::timeout(self.block_timeout, sink.send(Ok(body))).await,
            None => Ok(Err(FifoClosedError)),
        };

        let do_close = match result {
            Ok(Ok(())) => {
                if is_last {
                    *body_sink = None;
                }
                e.get().is_dead()
            }
            Ok(Err(FifoClosedError)) => {
                // If this is a request tunnel, it's much nicer to wait for the response first.
                // The response might tell the original HTTP client *why* we're not reading request
                // frames (e.g. early 404).
                response.is_none()
            }
            Err(_elapsed) => {
                tracing::warn!(
                    tunnel_id = ?id,
                    tunnel = ?e.get(),
                    timeout = ?self.block_timeout,
                    "Failed to send remote HTTP body frame through a local tunnel \
                    within the configured timeout. \
                    Closing the tunnel.",
                );
                true
            }
        };

        if do_close {
            e.remove();
            self.tunnels.smart_shrink();
            Ok(id.close_message().into())
        } else {
            Ok(Default::default())
        }
    }

    /// Consumes HTTP request body error from the [`mirrord_protocol`] server.
    pub async fn server_request_body_error(
        &mut self,
        id: TunnelId,
        message: Option<String>,
    ) -> TaskResult<OutBox> {
        let Entry::Occupied(mut e) = self.tunnels.entry(id) else {
            return Ok(Default::default());
        };
        let Tunnel::Request { body_sink, .. } = e.get_mut() else {
            return Err(TaskError::ProtocolViolation(
                format!("sent request body error to a raw connection ({id:?})").into(),
            ));
        };

        if let Some(tx) = body_sink {
            let _ = time::timeout(self.block_timeout, tx.send(Err(BodyError(message)))).await;
        }

        e.remove();
        self.tunnels.smart_shrink();
        Ok(id.close_message().into())
    }

    pub fn server_connection_lost(&mut self) {
        self.tunnels = Default::default();
        self.streams = Default::default();
    }

    /// Consumes an internal stream event.
    pub fn handle_event(&mut self, id: TunnelId, event: StreamEvent) -> OutBox {
        let Entry::Occupied(mut e) = self.tunnels.entry(id) else {
            return Default::default();
        };

        let mut out = OutBox::default();

        match e.get_mut() {
            Tunnel::Raw(tunnel) => match event {
                StreamEvent::InboundDataClosed => {
                    tunnel.inbound = None;
                    if tunnel.is_dead() {
                        e.remove();
                        self.tunnels.smart_shrink();
                        out.push(id.close_message());
                    }
                }

                StreamEvent::OutboundData(bytes) => out.extend(id.write_message(bytes)),

                StreamEvent::OutboundDataClosed => {
                    tunnel.outbound = None;
                    if tunnel.is_dead() {
                        e.remove();
                        self.tunnels.smart_shrink();
                        out.push(id.close_message());
                    } else {
                        out.extend(id.write_message(Default::default()));
                    }
                }

                StreamEvent::Response(..)
                | StreamEvent::ResponseBodyChunk(..)
                | StreamEvent::ResponseBodyError
                | StreamEvent::InboundBodyClosed => {
                    unreachable!("HTTP events are never produced for raw connections")
                }
            },

            Tunnel::Request {
                port,
                body_sink,
                response,
                upgrade,
            } => match event {
                StreamEvent::Response(internal_response) => {
                    if internal_response.body.is_finished() {
                        *response = None;
                    }

                    if internal_response.status == StatusCode::SWITCHING_PROTOCOLS {
                        let tunnel = match std::mem::replace(upgrade, UpgradeTunnel::NoUpgrade) {
                            UpgradeTunnel::Pending(tx) => {
                                let (tunnel, tunneled_data) = TrafficTunnels::create_tunnel(
                                    &mut self.streams,
                                    id,
                                    self.connection_capacity,
                                );
                                tx.send(tunneled_data).ok();
                                tunnel
                            }
                            UpgradeTunnel::Active(tunnel) => tunnel,
                            UpgradeTunnel::NoUpgrade => unreachable!(
                                "upgrade state never moves to NoUpgrade before we process the response head"
                            ),
                        };
                        *upgrade = UpgradeTunnel::Active(tunnel);
                    } else {
                        *upgrade = UpgradeTunnel::NoUpgrade;
                    }

                    match internal_response.body {
                        _ if id.0.has_outbound().not() => {}
                        InternalResponseBody::Legacy(body) => out.push(ClientMessage::TcpSteal(
                            LayerTcpSteal::HttpResponse(HttpResponse {
                                port: *port,
                                connection_id: id.1,
                                request_id: 0,
                                internal_response: InternalHttpResponse {
                                    status: internal_response.status,
                                    version: internal_response.version,
                                    headers: internal_response.headers,
                                    body,
                                },
                            }),
                        )),
                        InternalResponseBody::Framed(body) => out.push(ClientMessage::TcpSteal(
                            LayerTcpSteal::HttpResponseFramed(HttpResponse {
                                port: *port,
                                connection_id: id.1,
                                request_id: 0,
                                internal_response: InternalHttpResponse {
                                    status: internal_response.status,
                                    version: internal_response.version,
                                    headers: internal_response.headers,
                                    body,
                                },
                            }),
                        )),
                        InternalResponseBody::Chunked(chunk) => {
                            out.push(ClientMessage::TcpSteal(LayerTcpSteal::HttpResponseChunked(
                                ChunkedResponse::Start(HttpResponse {
                                    port: *port,
                                    connection_id: id.1,
                                    request_id: 0,
                                    internal_response: InternalHttpResponse {
                                        status: internal_response.status,
                                        version: internal_response.version,
                                        headers: internal_response.headers,
                                        body: chunk.frames,
                                    },
                                }),
                            )));
                            if chunk.is_last {
                                out.push(ClientMessage::TcpSteal(
                                    LayerTcpSteal::HttpResponseChunked(ChunkedResponse::Body(
                                        ChunkedRequestBodyV1 {
                                            frames: Default::default(),
                                            is_last: true,
                                            connection_id: id.1,
                                            request_id: 0,
                                        },
                                    )),
                                ));
                            }
                        }
                    };

                    if e.get().is_dead() {
                        e.remove();
                        self.tunnels.smart_shrink();
                        out.push(id.close_message());
                    }
                }

                StreamEvent::ResponseBodyChunk(internal_body) => {
                    if internal_body.is_last {
                        *response = None;
                    }

                    if id.0.has_outbound() {
                        out.push(ClientMessage::TcpSteal(LayerTcpSteal::HttpResponseChunked(
                            ChunkedResponse::Body(ChunkedRequestBodyV1 {
                                frames: internal_body.frames,
                                is_last: internal_body.is_last,
                                connection_id: id.1,
                                request_id: 0,
                            }),
                        )));
                    }

                    if e.get().is_dead() {
                        e.remove();
                        self.tunnels.smart_shrink();
                        out.push(id.close_message());
                    }
                }

                StreamEvent::ResponseBodyError => {
                    e.remove();
                    self.tunnels.smart_shrink();
                    out.push(ClientMessage::TcpSteal(LayerTcpSteal::HttpResponseChunked(
                        ChunkedResponse::Error(ChunkedRequestErrorV1 {
                            connection_id: id.1,
                            request_id: 0,
                        }),
                    )));
                    out.push(id.close_message())
                }

                StreamEvent::InboundBodyClosed => {
                    *body_sink = None;
                    if e.get().is_dead() {
                        e.remove();
                        self.tunnels.smart_shrink();
                        out.push(id.close_message());
                    }
                }

                StreamEvent::InboundDataClosed => {
                    let UpgradeTunnel::Active(tunnel) = upgrade else {
                        unreachable!();
                    };
                    tunnel.inbound = None;
                    if e.get().is_dead() {
                        e.remove();
                        self.tunnels.smart_shrink();
                        out.push(id.close_message());
                    }
                }

                StreamEvent::OutboundData(bytes) => out.extend(id.write_message(bytes)),

                StreamEvent::OutboundDataClosed => {
                    let UpgradeTunnel::Active(tunnel) = upgrade else {
                        unreachable!();
                    };
                    tunnel.outbound = None;
                    if e.get().is_dead() {
                        e.remove();
                        self.tunnels.smart_shrink();
                        out.push(id.close_message());
                    } else {
                        out.extend(id.write_message(Default::default()));
                    }
                }
            },
        }

        out
    }
}

impl Stream for TrafficTunnels {
    type Item = OutBox;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        loop {
            match std::task::ready!(this.streams.poll_next_unpin(cx)) {
                Some((id, event)) => {
                    let messages = this.handle_event(id, event);
                    if messages.is_empty().not() {
                        break Poll::Ready(Some(messages));
                    }
                }
                None => break Poll::Ready(None),
            }
        }
    }
}

/// Half of the state of a local traffic tunnel.
///
/// The other half lives in streams in [`TrafficTunnels::streams`].
enum Tunnel {
    /// Tunnel that originates from a raw connection.
    Raw(DataTunnel),
    /// Tunnel that originates from an HTTP request.
    Request {
        /// Original destination port of the request.
        ///
        /// Used when creating [`ClientMessage`]s.
        port: u16,
        /// Handle to the request body [`Fifo`].
        ///
        /// [`None`] if the [`Fifo`] has been closed.
        body_sink: Option<WatchedSink<Result<InternalHttpBodyNew, BodyError>>>,
        /// Handle to the response stream that lives [`TrafficTunnels::streams`].
        ///
        /// [`None`] if the stream has been finished.
        response: Option<AbortOnDrop>,
        /// State of the HTTP upgrade for this request.
        upgrade: UpgradeTunnel,
    },
}

impl Tunnel {
    /// Returns whether this tunnel is dead and can be removed from the state.
    fn is_dead(&self) -> bool {
        match self {
            Self::Raw(tunnel) => tunnel.is_dead(),
            Self::Request {
                body_sink: request_body_tx,
                response,
                upgrade,
                ..
            } => request_body_tx.is_none() && response.is_none() && upgrade.is_dead(),
        }
    }
}

impl fmt::Debug for Tunnel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Raw(tunnel) => f.debug_tuple("Raw").field(tunnel).finish(),
            Self::Request {
                port,
                body_sink,
                response,
                upgrade,
            } => f
                .debug_struct("Request")
                .field("port", port)
                .field("body_sink", body_sink)
                .field("response_finished", &response.is_none())
                .field("upgrade", upgrade)
                .finish(),
        }
    }
}

/// State of a local raw bytes traffic tunnel.
#[derive(Debug)]
struct DataTunnel {
    /// Handle to the remote->local data [`Fifo`].
    ///
    /// [`None`] if the [`Fifo`] has been closed.
    inbound: Option<WatchedSink<Bytes>>,
    /// Handle to the local->remote data stream that lives in [`TrafficTunnels::streams`].
    ///
    /// [`None`] if the stream has finished.
    outbound: Option<AbortOnDrop>,
}

impl DataTunnel {
    /// Processes inbound data from the server.
    async fn server_data(&mut self, data: Bytes) -> Result<(), FifoClosedError> {
        match self.inbound.as_mut() {
            Some(tx) => tx.send(data).await,
            None => Err(FifoClosedError),
        }
    }

    /// Returns whether this tunnel is dead (closed both ways).
    fn is_dead(&self) -> bool {
        self.inbound.is_none() && self.outbound.is_none()
    }
}

/// State of an HTTP upgrade of a tunneled request.
///
/// Each new request starts in [`UpgradeTunnel::Pending`].
/// We don't make the upgrade/no-upgrade decision based on request headers
/// because we don't need to. And I'm scared of nasty unknown edge cases.
///
/// Transition logic is as follows:
/// 1. [`UpgradeTunnel::Pending`] -> [`UpgradeTunnel::Active`] when we receive upgrade data from the
///    server
/// 2. [`UpgradeTunnel::Pending`] -> [`UpgradeTunnel::Active`] when we receive the request head with
///    [`StatusCode::SWITCHING_PROTOCOLS`]
/// 3. [`UpgradeTunnel::Pending`]/[`UpgradeTunnel::Active`] -> [`UpgradeTunnel::NoUpgrade`] when we
///    receive the request head with some other status code.
#[derive(Debug)]
enum UpgradeTunnel {
    /// Upgrade is pending.
    ///
    /// This means that:
    /// 1. We haven't processed the response head yet; AND
    /// 2. [`mirrord_protocol`] has not sent any upgrade data yet.
    Pending(oneshot::Sender<TunneledData>),
    /// Connection was upgraded.
    ///
    /// This means that either:
    /// 1. We have processed the response head, with [`StatusCode::SWITCHING_PROTOCOLS`]; OR
    /// 2. [`mirrord_protocol`] server has sent upgrade data.
    Active(DataTunnel),
    /// Connection was not upgraded.
    ///
    /// This means that we have processed the response head, and status code was not
    /// [`StatusCode::SWITCHING_PROTOCOLS`].
    NoUpgrade,
}

impl UpgradeTunnel {
    /// Returns whether this tunnel is dead (closed both ways).
    fn is_dead(&self) -> bool {
        match self {
            Self::Pending(..) => false,
            Self::Active(tunnel) => tunnel.is_dead(),
            Self::NoUpgrade => true,
        }
    }
}

/// Wrapper over a [`FifoSink`] that is monitored in [`TrafficTunnels::streams`].
///
/// Each [`FifoSink`] we use has a related stream that notifies us when the [`Fifo`] is closed.
/// This allows us to detect the following scenario:
/// 1. Server initiates new connection
/// 2. Client closes outbound data [`Fifo`] -> send shutdown to server
/// 3. Some time passes
/// 4. Client drops inbound data [`Fifo`]
///
/// If we were not watching the inbound [`FifoSink`]s,
/// we would only learn about 4. when (if) the server sends any data.
#[derive(Debug)]
struct WatchedSink<T> {
    sink: FifoSink<T>,
    _guard: AbortOnDrop,
}

impl<T> Deref for WatchedSink<T> {
    type Target = FifoSink<T>;

    fn deref(&self) -> &Self::Target {
        &self.sink
    }
}

impl<T> DerefMut for WatchedSink<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.sink
    }
}
