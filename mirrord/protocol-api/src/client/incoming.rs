use std::{
    collections::{HashMap, VecDeque, hash_map::Entry},
    fmt,
    net::{Ipv4Addr, SocketAddr},
    num::NonZeroUsize,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use futures::{
    SinkExt, Stream, StreamExt,
    stream::{AbortHandle, Abortable, Aborted, FuturesUnordered},
};
use mirrord_protocol::{
    ClientMessage, DaemonMessage, RemoteResult,
    tcp::{
        ChunkedRequest, ChunkedRequestStartV2, DaemonTcp, HttpFilter, HttpRequestMetadata,
        IncomingTrafficTransportType, InternalHttpBodyFrame, InternalHttpBodyNew, LayerTcp,
        LayerTcpSteal, NewTcpConnectionV2, StealType,
    },
};
use strum::VariantArray;
use strum_macros::VariantArray;
use tokio::time;

use crate::{
    client::{
        ClientError,
        enum_map::{EnumKey, EnumMap},
        error::{TaskError, TaskResult},
        outbox::OutBox,
        request::ResponseOneshot,
        tunnels::{TrafficTunnels, TunnelId, TunnelType},
    },
    fifo::{Fifo, FifoClosed, FifoClosedError, FifoSink, FifoStream},
    shrinkable::Shrinkable,
    tagged::Tagged,
    traffic::{TunneledIncoming, TunneledIncomingInner},
};

/// Mode of an incoming traffic subscription.
#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy, VariantArray)]
#[repr(u8)]
pub enum IncomingMode {
    Steal,
    Mirror,
}

impl From<IncomingMode> for TunnelType {
    fn from(value: IncomingMode) -> Self {
        match value {
            IncomingMode::Steal => Self::IncomingSteal,
            IncomingMode::Mirror => Self::IncomingMirror,
        }
    }
}

impl EnumKey for IncomingMode {
    fn into_index(self) -> usize {
        self as usize
    }
}

impl IncomingMode {
    /// Produces a port subscribe [`ClientMessage`].
    fn subscribe_message(self, port: u16, filter: Option<HttpFilter>) -> ClientMessage {
        match (self, filter) {
            (Self::Steal, None) => {
                ClientMessage::TcpSteal(LayerTcpSteal::PortSubscribe(StealType::All(port)))
            }
            (Self::Steal, Some(filter)) => ClientMessage::TcpSteal(LayerTcpSteal::PortSubscribe(
                StealType::FilteredHttpEx(port, filter),
            )),
            (Self::Mirror, None) => ClientMessage::Tcp(LayerTcp::PortSubscribe(port)),
            (Self::Mirror, Some(filter)) => {
                ClientMessage::Tcp(LayerTcp::PortSubscribeFilteredHttp(port, filter))
            }
        }
    }

    /// Produces a port unsubscribe [`ClientMessage`].
    fn unsubscribe_message(self, port: u16) -> ClientMessage {
        match self {
            Self::Steal => ClientMessage::TcpSteal(LayerTcpSteal::PortUnsubscribe(port)),
            Self::Mirror => ClientMessage::Tcp(LayerTcp::PortUnsubscribe(port)),
        }
    }
}

type IncomingModeMap<T> = EnumMap<IncomingMode, T, { IncomingMode::VARIANTS.len() }>;

/// State of port subscriptions withing a single [`IncomingMode`].
#[derive(Default, Debug)]
struct Subscriptions {
    /// Pending and confirmed subscriptions, by port.
    ///
    /// Within a single [`IncomingMode`] and port we can only have one subscription.
    by_port: HashMap<u16, Subscription>,
    /// Queued port subscribe [`ClientMessage`]s.
    ///
    /// Each time we issue a port subscription, we [`VecDeque::push_back`] the port number here.
    /// Each time we receive a port subscription response, we get the port number with
    /// [`VecDeque::pop_front`]. This queue is used **only** because the failure response does not
    /// contain the number of the port.
    ///
    /// This queue works because the server is expected to process port subscription requests in
    /// order.
    queued: VecDeque<u16>,
}

/// [`Future`] that resolves when the subscription channel is closed,
/// returning the mode and port of the subscription.
type SubscriptionClosed = Tagged<FifoClosed<TunneledIncoming>, (IncomingMode, u16)>;

/// Manages state of the incoming traffic feature.
///
/// Should be polled with [`Stream::poll_next`] for outbound [`ClientMessage`]s.
pub struct Incoming {
    /// State of port subscriptions.
    subscriptions: IncomingModeMap<Subscriptions>,
    /// Monitors active port subscriptions, yielding closed subscriptions.
    ///
    /// This is our way to know when to issue port unsubscribes.
    closed: FuturesUnordered<Abortable<SubscriptionClosed>>,
    /// Capacity for all subscription [`Fifo`]s.
    channel_capacity: NonZeroUsize,
    /// How long we're allowed to block when sending new traffic to the client.
    block_timeout: Duration,
}

impl Incoming {
    /// Creates a new instance.
    ///
    /// # Params
    /// * `channel_capacity` will be used to create subscription [`Fifo`]s (see
    ///   [`Self::handle_subscribe`])
    /// * `block_timeout` determines how long we're allowed to block when sending new traffic to the
    ///   client
    pub fn new(channel_capacity: NonZeroUsize, block_timeout: Duration) -> Self {
        Self {
            subscriptions: Default::default(),
            closed: Default::default(),
            channel_capacity,
            block_timeout,
        }
    }

    /// Handles a port subscribe request from a client.
    ///
    /// Once the subscription request completes, result will be send through the given `result_tx`.
    /// The result will be either an error or a [`FifoStream`] for receiving new traffic
    /// (connections or requests).
    pub fn handle_subscribe(
        &mut self,
        mode: IncomingMode,
        port: u16,
        filter: Option<HttpFilter>,
        result_tx: ResponseOneshot<FifoStream<TunneledIncoming>>,
    ) -> OutBox {
        let mut out = OutBox::default();

        let subscriptions = &mut self.subscriptions[mode];
        match subscriptions.by_port.entry(port) {
            Entry::Occupied(mut e) => match e.get_mut() {
                Subscription::Confirmed {
                    abort_closed_fut, ..
                } => {
                    abort_closed_fut.abort();
                    e.insert(Subscription::Pending {
                        result_tx,
                        waiting_to_overwrite: None,
                    });
                    subscriptions.queued.push_back(port);
                    out.push(mode.unsubscribe_message(port));
                    out.push(mode.subscribe_message(port, filter));
                }

                Subscription::Pending {
                    result_tx: prev_result_tx,
                    waiting_to_overwrite,
                } => {
                    let prev_result_tx = std::mem::replace(prev_result_tx, result_tx);
                    prev_result_tx
                        .send(Ok(Fifo::with_capacity(self.channel_capacity).stream))
                        .ok();
                    waiting_to_overwrite.replace(filter);
                }
            },

            Entry::Vacant(e) => {
                e.insert(Subscription::Pending {
                    result_tx,
                    waiting_to_overwrite: None,
                });
                subscriptions.queued.push_back(port);
                out.push(mode.subscribe_message(port, filter));
            }
        }

        out
    }

    /// Handles a message from the server.
    pub async fn handle_server_message(
        &mut self,
        mode: IncomingMode,
        message: DaemonTcp,
        tunnels: &mut TrafficTunnels,
    ) -> TaskResult<OutBox> {
        match message {
            DaemonTcp::SubscribeResult(result) => self.handle_subscribe_result(mode, result),

            DaemonTcp::Data(data) => {
                let id = TunnelId(mode.into(), data.connection_id);
                Ok(tunnels.server_data(id, data.bytes.0).await)
            }

            DaemonTcp::NewConnectionV1(connection) => {
                self.handle_new_connection(
                    mode,
                    NewTcpConnectionV2 {
                        connection,
                        transport: IncomingTrafficTransportType::Tcp,
                    },
                    tunnels,
                )
                .await?;
                Ok(Default::default())
            }

            DaemonTcp::NewConnectionV2(connection) => {
                self.handle_new_connection(mode, connection, tunnels)
                    .await?;
                Ok(Default::default())
            }

            DaemonTcp::Close(close) => {
                let id = TunnelId(mode.into(), close.connection_id);
                tunnels.server_close(id);
                Ok(Default::default())
            }

            DaemonTcp::HttpRequest(request) => {
                let request = ChunkedRequestStartV2 {
                    connection_id: request.connection_id,
                    request_id: request.request_id,
                    metadata: Self::dummy_request_metadata(request.port),
                    transport: IncomingTrafficTransportType::Tcp,
                    request: request
                        .internal_request
                        .map_body(|body| InternalHttpBodyNew {
                            frames: vec![InternalHttpBodyFrame::Data(body)],
                            is_last: true,
                        }),
                };
                self.handle_new_request(mode, request, tunnels).await?;
                Ok(Default::default())
            }

            DaemonTcp::HttpRequestFramed(request) => {
                let request = ChunkedRequestStartV2 {
                    connection_id: request.connection_id,
                    request_id: request.request_id,
                    metadata: Self::dummy_request_metadata(request.port),
                    transport: IncomingTrafficTransportType::Tcp,
                    request: request
                        .internal_request
                        .map_body(|frames| InternalHttpBodyNew {
                            frames: frames.0.into(),
                            is_last: true,
                        }),
                };
                self.handle_new_request(mode, request, tunnels).await?;
                Ok(Default::default())
            }

            DaemonTcp::HttpRequestChunked(message) => match message {
                ChunkedRequest::StartV1(request) => {
                    let request = ChunkedRequestStartV2 {
                        connection_id: request.connection_id,
                        request_id: request.request_id,
                        metadata: Self::dummy_request_metadata(request.port),
                        transport: IncomingTrafficTransportType::Tcp,
                        request: request
                            .internal_request
                            .map_body(|frames| InternalHttpBodyNew {
                                frames,
                                is_last: false,
                            }),
                    };
                    self.handle_new_request(mode, request, tunnels).await?;
                    Ok(Default::default())
                }
                ChunkedRequest::StartV2(request) => {
                    self.handle_new_request(mode, request, tunnels).await?;
                    Ok(Default::default())
                }
                ChunkedRequest::Body(body) => {
                    if body.request_id != 0 {
                        return Err(TaskError::ProtocolViolation(
                            "use non-zero request id".into(),
                        ));
                    }
                    tunnels
                        .server_request_body(
                            TunnelId(mode.into(), body.connection_id),
                            InternalHttpBodyNew {
                                frames: body.frames,
                                is_last: body.is_last,
                            },
                        )
                        .await
                }
                ChunkedRequest::ErrorV1(error) => {
                    if error.request_id != 0 {
                        return Err(TaskError::ProtocolViolation(
                            "use non-zero request id".into(),
                        ));
                    }
                    tunnels
                        .server_request_body_error(TunnelId(mode.into(), error.connection_id), None)
                        .await
                }
                ChunkedRequest::ErrorV2(error) => {
                    if error.request_id != 0 {
                        return Err(TaskError::ProtocolViolation(
                            "use non-zero request id".into(),
                        ));
                    }
                    tunnels
                        .server_request_body_error(
                            TunnelId(mode.into(), error.connection_id),
                            Some(error.error_message),
                        )
                        .await
                }
            },
        }
    }

    pub fn server_connection_lost(&mut self, error: &TaskError) {
        std::mem::take(&mut self.subscriptions)
            .into_values()
            .into_iter()
            .flat_map(|subscriptions| subscriptions.by_port.into_values())
            .for_each(|subscription| {
                if let Subscription::Pending {
                    result_tx: traffic_tx,
                    ..
                } = subscription
                {
                    traffic_tx
                        .send(Err(ClientError::ConnectionLost(error.clone())))
                        .ok();
                }
            });
        self.closed = Default::default();
    }

    fn dummy_request_metadata(destination_port: u16) -> HttpRequestMetadata {
        HttpRequestMetadata::V1 {
            source: SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 9999),
            destination: SocketAddr::new(Ipv4Addr::LOCALHOST.into(), destination_port),
        }
    }

    async fn handle_new_connection(
        &mut self,
        mode: IncomingMode,
        connection: NewTcpConnectionV2,
        tunnels: &mut TrafficTunnels,
    ) -> TaskResult<()> {
        let id = TunnelId(mode.into(), connection.connection.connection_id);
        let data = tunnels.server_connection(id)?;
        let traffic = TunneledIncoming {
            inner: TunneledIncomingInner::Raw(data),
            transport: connection.transport,
            remote_local_addr: SocketAddr::new(
                connection.connection.local_address,
                connection.connection.destination_port,
            ),
            remote_peer_addr: SocketAddr::new(
                connection.connection.remote_address,
                connection.connection.source_port,
            ),
        };

        if let Some(sub) = self.subscriptions[mode]
            .by_port
            .get_mut(&traffic.remote_local_addr.port())
        {
            sub.accept_traffic(traffic, self.block_timeout).await;
        };

        Ok(())
    }

    async fn handle_new_request(
        &mut self,
        mode: IncomingMode,
        request: ChunkedRequestStartV2,
        tunnels: &mut TrafficTunnels,
    ) -> TaskResult<()> {
        if request.request_id != 0 {
            return Err(TaskError::ProtocolViolation(
                "use non-zero request id".into(),
            ));
        }

        let HttpRequestMetadata::V1 {
            source,
            destination,
        } = request.metadata;
        let transport = request.transport;
        let port = destination.port();

        let request = tunnels.server_request(
            TunnelId(mode.into(), request.connection_id),
            port,
            request.request,
        )?;

        let traffic = TunneledIncoming {
            inner: TunneledIncomingInner::Http(request),
            transport,
            remote_local_addr: destination,
            remote_peer_addr: source,
        };

        if let Some(sub) = self.subscriptions[mode]
            .by_port
            .get_mut(&traffic.remote_local_addr.port())
        {
            sub.accept_traffic(traffic, self.block_timeout).await;
        };

        Ok(())
    }

    fn handle_subscribe_result(
        &mut self,
        mode: IncomingMode,
        result: RemoteResult<u16>,
    ) -> TaskResult<OutBox> {
        let mut out = OutBox::default();

        let subscriptions = &mut self.subscriptions[mode];
        let sub = subscriptions.queued.pop_front().and_then(|port| {
            subscriptions.queued.smart_shrink();
            if result.as_ref().is_ok_and(|got_port| *got_port != port) {
                return None;
            }
            let Entry::Occupied(mut e) = subscriptions.by_port.entry(port) else {
                return None;
            };
            let Subscription::Pending { .. } = e.get_mut() else {
                return None;
            };
            Some(e)
        });
        let Some(mut sub) = sub else {
            let message = match mode {
                IncomingMode::Mirror => DaemonMessage::Tcp(DaemonTcp::SubscribeResult(result)),
                IncomingMode::Steal => DaemonMessage::TcpSteal(DaemonTcp::SubscribeResult(result)),
            };
            return Err(TaskError::unexpected_message(&message));
        };

        let port = *sub.key();
        let Subscription::Pending {
            waiting_to_overwrite,
            ..
        } = sub.get_mut()
        else {
            unreachable!("was checked above");
        };

        match (result, waiting_to_overwrite.take()) {
            (Ok(..), Some(next)) => {
                subscriptions.queued.push_back(port);
                out.push(mode.unsubscribe_message(port));
                out.push(mode.subscribe_message(port, next));
            }

            (Ok(..), None) => {
                let traffic = Fifo::with_capacity(self.channel_capacity);

                let (abort_handle, abort_registration) = AbortHandle::new_pair();
                let future = Abortable::new(
                    Tagged {
                        inner: traffic.closed,
                        tag: (mode, port),
                    },
                    abort_registration,
                );
                self.closed.push(future);

                let Subscription::Pending { result_tx, .. } = sub.insert(Subscription::Confirmed {
                    traffic_tx: traffic.sink,
                    abort_closed_fut: abort_handle,
                }) else {
                    unreachable!("was checked above");
                };
                result_tx.send(Ok(traffic.stream)).ok();
            }

            (Err(..), Some(next)) => {
                subscriptions.queued.push_back(port);
                out.push(mode.subscribe_message(port, next));
            }

            (Err(error), None) => {
                let Subscription::Pending { result_tx, .. } = sub.remove() else {
                    unreachable!("was checked above");
                };
                result_tx.send(Err(ClientError::Response(error))).ok();
                subscriptions.by_port.smart_shrink();
            }
        }

        Ok(out)
    }
}

impl Stream for Incoming {
    type Item = OutBox;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        let (mode, port) = loop {
            match std::task::ready!(this.closed.poll_next_unpin(cx)) {
                None => return Poll::Ready(None),
                Some(Ok((values, ()))) => break values,
                Some(Err(Aborted)) => {}
            }
        };
        let map = &mut this.subscriptions[mode].by_port;
        map.remove(&port);
        map.smart_shrink();
        Poll::Ready(Some(mode.unsubscribe_message(port).into()))
    }
}

impl fmt::Debug for Incoming {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Incoming")
            .field("subscriptions", &self.subscriptions)
            .field("closed_futs", &self.closed.len())
            .field("channel_capacity", &self.channel_capacity)
            .field("block_timeout", &self.block_timeout)
            .finish()
    }
}

enum Subscription {
    Pending {
        result_tx: ResponseOneshot<FifoStream<TunneledIncoming>>,
        waiting_to_overwrite: Option<Option<HttpFilter>>,
    },
    Confirmed {
        traffic_tx: FifoSink<TunneledIncoming>,
        abort_closed_fut: AbortHandle,
    },
}

impl Subscription {
    async fn accept_traffic(&mut self, traffic: TunneledIncoming, timeout: Duration) {
        let Self::Confirmed { traffic_tx, .. } = self else {
            return;
        };

        match time::timeout(timeout, traffic_tx.send(traffic)).await {
            Ok(Ok(())) => {}
            Ok(Err(FifoClosedError)) => {}
            Err(_elapsed) => {
                if let Some(traffic) = traffic_tx.take_pending() {
                    tracing::warn!(
                        ?timeout,
                        ?traffic,
                        "Failed to send new incoming traffic through a local subscription tunnel \
                        within the configured timeout. Dropping the traffic.",
                    );
                }
            }
        }
    }
}

impl fmt::Debug for Subscription {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pending {
                waiting_to_overwrite,
                ..
            } => f
                .debug_struct("Pending")
                .field("waiting_to_override", &waiting_to_overwrite.is_some())
                .finish(),
            Self::Confirmed { traffic_tx, .. } => f
                .debug_struct("Confirmed")
                .field("traffic_sink", traffic_tx)
                .finish(),
        }
    }
}

#[cfg(test)]
mod test {
    use std::{net::Ipv4Addr, num::NonZeroUsize, time::Duration};

    use mirrord_protocol::{
        ClientMessage,
        tcp::{DaemonTcp, LayerTcp, NewTcpConnectionV1},
    };
    use tokio::sync::oneshot;
    use tokio_stream::StreamExt;

    use crate::client::{
        incoming::{Incoming, IncomingMode},
        tunnels::TrafficTunnels,
    };

    /// Verifies regular subscription flow:
    /// 1. Client issues a subscription
    /// 2. Server confirms
    /// 3. Server sends traffic
    /// 4. Client receives traffic
    #[tokio::test]
    async fn subscription_simple() {
        let mut tunnels =
            TrafficTunnels::new(NonZeroUsize::MAX, Duration::MAX, &mirrord_protocol::VERSION);
        let mut incoming = Incoming::new(NonZeroUsize::MAX, Duration::MAX);

        let (tx, rx) = oneshot::channel();
        assert_eq!(
            incoming.handle_subscribe(IncomingMode::Mirror, 80, None, tx),
            ClientMessage::Tcp(LayerTcp::PortSubscribe(80))
        );
        assert!(
            incoming
                .handle_server_message(
                    IncomingMode::Mirror,
                    DaemonTcp::SubscribeResult(Ok(80)),
                    &mut tunnels,
                )
                .await
                .unwrap()
                .is_empty()
        );
        let mut sub = rx.await.unwrap().unwrap();

        assert!(
            incoming
                .handle_server_message(
                    IncomingMode::Mirror,
                    DaemonTcp::NewConnectionV1(NewTcpConnectionV1 {
                        connection_id: 0,
                        remote_address: Ipv4Addr::LOCALHOST.into(),
                        destination_port: 80,
                        source_port: 90,
                        local_address: Ipv4Addr::LOCALHOST.into(),
                    }),
                    &mut tunnels,
                )
                .await
                .unwrap()
                .is_empty()
        );
        sub.next()
            .await
            .expect("subscription channel should receive traffic");

        drop(sub);

        assert_eq!(
            incoming.next().await.unwrap(),
            ClientMessage::Tcp(LayerTcp::PortUnsubscribe(80))
        );
        assert_eq!(incoming.next().await, None,);
    }

    /// Verifies subscription overwrite flow:
    /// 1. Client issues a subscription
    /// 2. Server confirms
    /// 3. Client issues another subscription of the same port and [`IncomingMode`]
    /// 4. Previous subscription is overwritten with unsubscribe+subscribe
    /// 5. Server confirms
    /// 6. Server sends traffic
    /// 7. Client receives traffic
    #[tokio::test]
    async fn subscription_overwrite() {
        let mut tunnels =
            TrafficTunnels::new(NonZeroUsize::MAX, Duration::MAX, &mirrord_protocol::VERSION);
        let mut incoming = Incoming::new(NonZeroUsize::MAX, Duration::MAX);

        let (tx, rx) = oneshot::channel();
        assert_eq!(
            incoming.handle_subscribe(IncomingMode::Mirror, 80, None, tx),
            ClientMessage::Tcp(LayerTcp::PortSubscribe(80))
        );
        assert!(
            incoming
                .handle_server_message(
                    IncomingMode::Mirror,
                    DaemonTcp::SubscribeResult(Ok(80)),
                    &mut tunnels,
                )
                .await
                .unwrap()
                .is_empty()
        );
        let mut sub_1 = rx.await.unwrap().unwrap();

        let (tx, rx_2) = oneshot::channel();
        assert_eq!(
            incoming.handle_subscribe(IncomingMode::Mirror, 80, None, tx),
            [
                ClientMessage::Tcp(LayerTcp::PortUnsubscribe(80)),
                ClientMessage::Tcp(LayerTcp::PortSubscribe(80)),
            ]
        );
        assert!(sub_1.next().await.is_none());

        assert!(
            incoming
                .handle_server_message(
                    IncomingMode::Mirror,
                    DaemonTcp::SubscribeResult(Ok(80)),
                    &mut tunnels,
                )
                .await
                .unwrap()
                .is_empty()
        );
        let mut sub_2 = rx_2.await.unwrap().unwrap();

        assert!(
            incoming
                .handle_server_message(
                    IncomingMode::Mirror,
                    DaemonTcp::NewConnectionV1(NewTcpConnectionV1 {
                        connection_id: 0,
                        remote_address: Ipv4Addr::LOCALHOST.into(),
                        destination_port: 80,
                        source_port: 90,
                        local_address: Ipv4Addr::LOCALHOST.into(),
                    }),
                    &mut tunnels,
                )
                .await
                .unwrap()
                .is_empty()
        );
        assert!(sub_2.next().await.is_some());

        drop(sub_2);
        assert_eq!(
            incoming.next().await.unwrap(),
            ClientMessage::Tcp(LayerTcp::PortUnsubscribe(80))
        );
        assert_eq!(incoming.next().await, None);
    }

    /// Verifies subscription rapid overwrite flow:
    /// 1. Client issues a subscription
    /// 2. Client issues another subscription of the same port and [`IncomingMode`]
    /// 3. Server confirms the first subscription
    /// 4. Previous subscription is overwritten with unsubscribe+subscribe
    /// 5. Server confirms the second subscription
    /// 6. Server sends traffic
    /// 7. Client receives traffic
    #[tokio::test]
    async fn subscription_overwrite_rapid() {
        let mut tunnels =
            TrafficTunnels::new(NonZeroUsize::MAX, Duration::MAX, &mirrord_protocol::VERSION);
        let mut incoming = Incoming::new(NonZeroUsize::MAX, Duration::MAX);

        let (tx, rx_1) = oneshot::channel();
        assert_eq!(
            incoming.handle_subscribe(IncomingMode::Mirror, 80, None, tx),
            ClientMessage::Tcp(LayerTcp::PortSubscribe(80))
        );

        let (tx, rx_2) = oneshot::channel();
        assert!(
            incoming
                .handle_subscribe(IncomingMode::Mirror, 80, None, tx)
                .is_empty()
        );
        let mut sub_1 = rx_1.await.unwrap().unwrap();
        assert!(sub_1.next().await.is_none());

        assert_eq!(
            incoming
                .handle_server_message(
                    IncomingMode::Mirror,
                    DaemonTcp::SubscribeResult(Ok(80)),
                    &mut tunnels,
                )
                .await
                .unwrap(),
            [
                ClientMessage::Tcp(LayerTcp::PortUnsubscribe(80)),
                ClientMessage::Tcp(LayerTcp::PortSubscribe(80)),
            ]
        );

        assert!(
            incoming
                .handle_server_message(
                    IncomingMode::Mirror,
                    DaemonTcp::SubscribeResult(Ok(80)),
                    &mut tunnels,
                )
                .await
                .unwrap()
                .is_empty()
        );
        let mut sub_2 = rx_2.await.unwrap().unwrap();

        assert!(
            incoming
                .handle_server_message(
                    IncomingMode::Mirror,
                    DaemonTcp::NewConnectionV1(NewTcpConnectionV1 {
                        connection_id: 0,
                        remote_address: Ipv4Addr::LOCALHOST.into(),
                        destination_port: 80,
                        source_port: 90,
                        local_address: Ipv4Addr::LOCALHOST.into(),
                    }),
                    &mut tunnels,
                )
                .await
                .unwrap()
                .is_empty()
        );
        assert!(sub_2.next().await.is_some());

        drop(sub_2);
        assert_eq!(
            incoming.next().await.unwrap(),
            ClientMessage::Tcp(LayerTcp::PortUnsubscribe(80))
        );
        assert_eq!(incoming.next().await, None);
    }
}
