use std::{
    collections::{HashMap, VecDeque, hash_map::Entry},
    fmt, io,
    ops::RangeInclusive,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use bytes::Bytes;
use futures::{
    Sink, SinkExt, Stream, StreamExt,
    stream::{AbortHandle, Abortable, SelectAll, Zip},
};
use mirrord_protocol::{ConnectionId, outgoing::SocketAddress, uid::Uid};
use tokio::{task::JoinSet, time::Instant};
use tokio_stream::StreamNotifyClose;
use tracing::{Level, field};

use crate::{
    outgoing::OutgoingError,
    task::BgTaskRuntime,
    util::io::{
        buffered::{UnboundedBufferedSink, UnboundedBufferedStream},
        throttle::{Throttle, Throttled, ThrottledSink, ThrottledStream},
        timeout::TimeoutSink,
    },
};

/// Core logic of agent's outgoing traffic.
///
/// Manages all outgoing connections of a single [`ConnectionKind`] for one client.
/// Does not handle [`mirrord_protocol`], uses generic [`RouterUpdate`]s and raw [`Bytes`] instead.
/// This is because we have multiple kinds of outgoing traffic, all using different message types.
///
/// # Making new connections
///
/// Connect attempts are run in the provided [`BgTaskRuntime`],
/// which **must** run in the target's network namespace (if this agent has a target).
/// All attempts are subject to [`Self::CONNECT_TIMEOUT`].
pub struct OutgoingRouter<C: ConnectionKind> {
    free_conn_ids: RangeInclusive<ConnectionId>,
    /// Read halves of open connections.
    streams: SelectAll<WrappedStream>,
    /// Write halves of open connections.
    sinks: HashMap<ConnectionId, WrappedSink>,
    /// Handles used to abort reading from connections.
    ///
    /// Each [`AbortHandle`] is linked to an [`Abortable`] stream in [`Self::streams`].
    /// This mechanic is used because [`SelectAll`] is not a proper map,
    /// we don't want to do O(1) scan each time we need to remove a stream.
    abort_handles: HashMap<ConnectionId, AbortHandle>,
    /// Ongoing connect attempts.
    ///
    /// These are spawned on the runtime from [`Self::network_runtime`] (with
    /// [`JoinSet::spawn_on`]).
    connects: JoinSet<(Uid, io::Result<NewConnection<C>>)>,
    /// Used to throttle reads from active connections.
    streams_throttle: Throttle,
    /// Used to throttle writes to active connections.
    sinks_throttle: Throttle,
    /// Updates ready to return from [`Self::recv`].
    queued_updates: VecDeque<RouterUpdate>,
    /// Total count of active connections.
    total_connections: usize,
    /// Runtime where we should run connect attempts.
    network_runtime: Arc<BgTaskRuntime>,
}

impl<C: ConnectionKind> OutgoingRouter<C> {
    /// Timeout for writing a data chunk into an outgoing connection.
    ///
    /// This timeout is very generous, but still nice to have.
    /// It protects us from hanging forever on a non-responsive connection.
    pub const SINK_TIMEOUT: Duration = Duration::from_secs(30);
    /// Timeout for making a new outgoing connection.
    pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

    pub fn new(
        network_runtime: Arc<BgTaskRuntime>,
        inbound_throttle: Throttle,
        outbound_throttle: Throttle,
    ) -> Self {
        Self {
            free_conn_ids: (ConnectionId::MIN..=ConnectionId::MAX),
            streams: Default::default(),
            sinks: Default::default(),
            abort_handles: Default::default(),
            connects: Default::default(),
            streams_throttle: inbound_throttle,
            sinks_throttle: outbound_throttle,
            queued_updates: Default::default(),
            total_connections: 0,
            network_runtime,
        }
    }

    #[tracing::instrument(
        level = Level::TRACE,
        skip(self),
        fields(kind = C::DISPLAY_NAME),
    )]
    pub fn start_connect(&mut self, uid: Uid, address: SocketAddress) {
        let target_pid = self.network_runtime.target_pid();

        let fut = async move {
            let start_at = Instant::now();
            let result =
                tokio::time::timeout(Self::CONNECT_TIMEOUT, C::connect(&address, target_pid))
                    .await
                    .unwrap_or_else(|_elapsed| Err(io::ErrorKind::TimedOut.into()))
                    .inspect_err(|error| {
                        tracing::warn!(
                            kind = C::DISPLAY_NAME,
                            peer_address = %address,
                            %error,
                            "Failed to make an outgoing connection",
                        );
                    })
                    .inspect(|conn| {
                        tracing::debug!(
                            kind = C::DISPLAY_NAME,
                            local_address = %conn.local_addr,
                            peer_address = %conn.peer_addr,
                            elapsed = ?start_at.elapsed(),
                            "Made an outgoing connection",
                        );
                    });
            (uid, result)
        };
        self.connects.spawn_on(fut, self.network_runtime.handle());
    }

    #[tracing::instrument(
        level = Level::TRACE,
        skip(self, data),
        fields(
            data = data.len(),
            kind = C::DISPLAY_NAME,
        ),
    )]
    pub async fn write(&mut self, id: ConnectionId, data: Bytes) {
        let Entry::Occupied(mut e) = self.sinks.entry(id) else {
            return;
        };
        let Err(error) = e.get_mut().send(data).await else {
            return;
        };
        e.remove();
        if let Some(handle) = self.abort_handles.remove(&id) {
            handle.abort();
        }
        self.decrement_open_conns_counter();
        self.queued_updates.push_back(RouterUpdate::ConnEvent {
            id,
            event: ConnEvent::Failed(error),
        });
        self.queued_updates.push_back(RouterUpdate::ConnEvent {
            id,
            event: ConnEvent::FullyClosed,
        });
    }

    #[tracing::instrument(
        level = Level::TRACE,
        skip(self),
        fields(kind = C::DISPLAY_NAME),
    )]
    pub async fn close_writing(&mut self, id: ConnectionId) {
        let Some(mut sink) = self.sinks.remove(&id) else {
            return;
        };
        match sink.close().await {
            Ok(()) if self.abort_handles.contains_key(&id) => {}
            Ok(()) => {
                self.decrement_open_conns_counter();
                self.queued_updates.push_back(RouterUpdate::ConnEvent {
                    id,
                    event: ConnEvent::FullyClosed,
                });
            }
            Err(error) => {
                if let Some(handle) = self.abort_handles.remove(&id) {
                    handle.abort();
                }
                self.decrement_open_conns_counter();
                self.queued_updates.push_back(RouterUpdate::ConnEvent {
                    id,
                    event: ConnEvent::Failed(error),
                });
                self.queued_updates.push_back(RouterUpdate::ConnEvent {
                    id,
                    event: ConnEvent::FullyClosed,
                });
            }
        }
    }

    #[tracing::instrument(
        level = Level::TRACE,
        skip(self),
        fields(kind = C::DISPLAY_NAME),
    )]
    pub async fn close(&mut self, id: ConnectionId) {
        let mut did_close = false;
        if let Some(mut sink) = self.sinks.remove(&id) {
            did_close = true;
            if let Err(error) = sink.close().await {
                self.queued_updates.push_back(RouterUpdate::ConnEvent {
                    id,
                    event: ConnEvent::Failed(error),
                });
            }
        }
        if let Some(abort_handle) = self.abort_handles.remove(&id) {
            abort_handle.abort();
            did_close = true;
        }
        if did_close {
            self.decrement_open_conns_counter();
        }
    }

    fn handle_new_connection(
        &mut self,
        uid: Uid,
        conn: NewConnection<C>,
    ) -> Result<RouterUpdate, OutgoingError> {
        let id = self
            .free_conn_ids
            .next()
            .ok_or(OutgoingError::ExhaustedConnIds)?;
        self.sinks.insert(
            id,
            ThrottledSink::new(
                UnboundedBufferedSink::new(TimeoutSink::new(conn.sink, Self::SINK_TIMEOUT)),
                self.sinks_throttle.clone(),
            ),
        );
        let stream = StreamNotifyClose::new(UnboundedBufferedStream::new(ThrottledStream::new(
            conn.stream,
            self.streams_throttle.clone(),
        )));
        let (stream, abort_handle) =
            futures::stream::abortable(futures::stream::repeat(id).zip(stream));
        self.streams.push(stream);
        self.abort_handles.insert(id, abort_handle);
        self.increment_open_conns_counter();
        Ok(RouterUpdate::ConnectOk {
            uid,
            id,
            local_addr: conn.local_addr,
            peer_addr: conn.peer_addr,
        })
    }

    #[tracing::instrument(
        level = Level::TRACE,
        skip(self, read),
        fields(
            kind = C::DISPLAY_NAME,
            read_amount = read.as_ref().and_then(|res| res.as_ref().ok()).map(|data| data.len()),
            read_error = read.as_ref().and_then(|res| res.as_ref().err()).map(field::display),
            // This might look dumb, but the result is:
            // 1. If `read` is `None`, span will have field `read_closed=true`
            // 2. Otherwise, span will not have this field
            read_closed = read.is_none().then_some(true),
        ),
        ret,
    )]
    fn handle_conn_read(
        &mut self,
        id: ConnectionId,
        read: Option<io::Result<Throttled<Bytes>>>,
    ) -> RouterUpdate {
        match read {
            None => {
                self.abort_handles.remove(&id);
                if self.sinks.contains_key(&id) {
                    RouterUpdate::ConnEvent {
                        id,
                        event: ConnEvent::ReadClosed,
                    }
                } else {
                    self.decrement_open_conns_counter();
                    RouterUpdate::ConnEvent {
                        id,
                        event: ConnEvent::FullyClosed,
                    }
                }
            }
            Some(Ok(data)) => RouterUpdate::ConnEvent {
                id,
                event: ConnEvent::ReadData(data),
            },
            Some(Err(error)) => {
                self.sinks.remove(&id);
                self.abort_handles.remove(&id);
                self.decrement_open_conns_counter();
                self.queued_updates.push_back(RouterUpdate::ConnEvent {
                    id,
                    event: ConnEvent::FullyClosed,
                });
                RouterUpdate::ConnEvent {
                    id,
                    event: ConnEvent::Failed(error),
                }
            }
        }
    }

    fn decrement_open_conns_counter(&mut self) {
        self.total_connections -= 1;
        C::conn_counter().fetch_sub(1, Ordering::Relaxed);
    }

    fn increment_open_conns_counter(&mut self) {
        self.total_connections += 1;
        C::conn_counter().fetch_add(1, Ordering::Relaxed);
    }

    /// Receives the next update from this router.
    ///
    /// # Cancellation safety
    ///
    /// This method is cancel safe, and can be used in a [`tokio::select`] branch.
    pub async fn recv(&mut self) -> Option<Result<RouterUpdate, OutgoingError>> {
        if let Some(update) = self.queued_updates.pop_front() {
            return Some(Ok(update));
        }

        tokio::select! {
            Some(result) = self.connects.join_next() => match result {
                Ok((uid, Ok(conn))) => Some(self.handle_new_connection(uid, conn)),
                Ok((uid, Err(error))) => Some(Ok(RouterUpdate::ConnectErr { uid, error })),
                Err(error) => Some(Err(OutgoingError::ConnectPanic(error))),
            },
            Some((id, read)) = self.streams.next() => Some(Ok(self.handle_conn_read(id, read))),
            else => None,
        }
    }
}

impl<C: ConnectionKind> Drop for OutgoingRouter<C> {
    fn drop(&mut self) {
        C::conn_counter().fetch_sub(self.total_connections, Ordering::Relaxed);
    }
}

pub trait ConnectionKind: 'static {
    type Sink: Sink<Throttled<Bytes>, Error = io::Error> + Send + Unpin + 'static;
    type Stream: Stream<Item = io::Result<Bytes>> + Send + Unpin + 'static;

    const DISPLAY_NAME: &'static str;

    /// Attempts to make a connection to the given peer.
    fn connect(
        addr: &SocketAddress,
        target_pid: Option<u64>,
    ) -> impl Future<Output = io::Result<NewConnection<Self>>> + Send;

    /// Returns the kind-specific prometheus metrics counter of active outgoing connections.
    fn conn_counter() -> &'static AtomicUsize;
}

/// Update from the [`OutgoingRouter`].
#[derive(Debug)]
pub enum RouterUpdate {
    /// Something happened in one of the managed connections.
    ConnEvent { id: ConnectionId, event: ConnEvent },
    /// Connect attempt succeeded.
    ConnectOk {
        uid: Uid,
        id: ConnectionId,
        local_addr: SocketAddress,
        peer_addr: SocketAddress,
    },
    /// Connect attempt failed.
    ConnectErr { uid: Uid, error: io::Error },
}

/// Event from an outgoing connection, produced by the [`OutgoingRouter`].
pub enum ConnEvent {
    ReadData(Throttled<Bytes>),
    ReadClosed,
    Failed(io::Error),
    FullyClosed,
}

impl fmt::Debug for ConnEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadData(data) => write!(f, "ReadData({} bytes)", data.len()),
            Self::ReadClosed => f.write_str("ReadClosed"),
            Self::Failed(error) => write!(f, "Failed({error:?})"),
            Self::FullyClosed => f.write_str("FullyClosed"),
        }
    }
}

/// Type of [`Sink`] used by the [`OutgoingRouter`] to handle client->peers connection data.
///
/// Multiple wrappers on top of the kind-specific implementation.
/// Data goes through:
/// 1. [`ThrottledSink`] - applies throttling as soon as possible
/// 2. [`UnboundedBufferedSink`] - flushes the data in the background, even when the router is not
///    polled
/// 3. [`TimeoutSink`] - ensures that we don't block forever trying to write to a non-responsive
///    peer
/// 4. [`ConnectionKind::Sink`] - specific to the exact outgoing flavor
///
/// Note that 3. and 4. are obscured by [`UnboundedBufferedSink`],
/// which erases the type of the [`Sink`] it wraps.
type WrappedSink = ThrottledSink<Bytes, UnboundedBufferedSink<Throttled<Bytes>>>;

/// Type of [`Stream`] used by the [`OutgoingRouter`] to handle peers->client connection data.
///
/// Multiple wrappers on top of the kind-specific implementation.
/// Data goes through:
/// 1. [`ConnectionKind::Stream`] - specific to the exact outgoing flavor
/// 2. [`ThrottledStream`] - applies throttling as soon as possible
/// 3. [`UnboundedBufferedStream`] - reads the data in the background, even when the router is not
///    polled
/// 4. [`StreamNotifyClose`] - notifies the router about write shutdown from the peer
/// 5. [`Abortable`] - allows for aborting the stream with an [`AbortHandle`]
///    ([`OutgoingRouter::abort_handles`]), for example if the client closes the connection
///
/// Note that 1. and 2. are obscured by [`UnboundedBufferedStream`],
/// which erases the type of the [`Stream`] it wraps.
type WrappedStream = Abortable<
    Zip<
        futures::stream::Repeat<ConnectionId>,
        StreamNotifyClose<UnboundedBufferedStream<Throttled<Bytes>>>,
    >,
>;

/// Newly established outgoing connection.
pub struct NewConnection<C: ConnectionKind + ?Sized> {
    pub stream: C::Stream,
    pub sink: C::Sink,
    pub local_addr: SocketAddress,
    pub peer_addr: SocketAddress,
}

#[cfg(test)]
mod test {
    use std::{io, sync::Arc, time::Duration};

    use bytes::Bytes;
    use mirrord_protocol::uid::Uid;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpSocket},
        sync::watch,
    };

    use crate::{
        outgoing::{
            router::{ConnEvent, OutgoingRouter, RouterUpdate},
            tcp_unix::TcpOrUnixConnection,
        },
        task::BgTaskRuntime,
        util::io::throttle::Throttle,
    };

    async fn test_router() -> OutgoingRouter<TcpOrUnixConnection> {
        OutgoingRouter::<TcpOrUnixConnection>::new(
            Arc::new(BgTaskRuntime::spawn(None).await.unwrap()),
            Throttle::new(128 * 1024),
            Throttle::new(128 * 1024),
        )
    }

    #[tokio::test]
    async fn connect_timeout() {
        let mut router = test_router().await;
        let socket = TcpSocket::new_v4().unwrap();
        socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let listener = socket.listen(1).unwrap();

        loop {
            let uid = Uid::new_v4();
            router.start_connect(uid, listener.local_addr().unwrap().into());
            let update = router.recv().await.unwrap().unwrap();
            match update {
                RouterUpdate::ConnectOk { uid: got_uid, .. } => assert_eq!(got_uid, uid),
                RouterUpdate::ConnectErr {
                    uid: got_uid,
                    error,
                } => {
                    assert_eq!(got_uid, uid);
                    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
                    break;
                }
                other => panic!("unexpected update {other:?}"),
            }
        }
    }

    const DATA: &[u8] = &[b'A'; 64 * 1024];

    #[tokio::test]
    async fn throttling_outbound() {
        let mut router = test_router().await;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        router.start_connect(Uid::new_v4(), listener.local_addr().unwrap().into());
        let (mut conn, ..) = listener.accept().await.unwrap();
        let RouterUpdate::ConnectOk { id, .. } = router.recv().await.unwrap().unwrap() else {
            unreachable!();
        };

        let data = Bytes::from_static(DATA);
        let (sent_chunks_tx, mut sent_chunks_rx) = watch::channel(0);
        let client_fut = async {
            loop {
                tokio::select! {
                    _ = sent_chunks_tx.closed() => break,
                    _ = router.write(id, data.clone()) => {},
                }
                sent_chunks_tx.send_modify(|count| *count += 1);
            }
        };
        let peer_fut = async {
            let throttled_at = loop {
                let res =
                    tokio::time::timeout(Duration::from_secs(2), sent_chunks_rx.changed()).await;
                if res.is_err() {
                    break *sent_chunks_rx.borrow();
                }
            };

            let mut consumed_data = 0;
            let mut buffer = Vec::<u8>::with_capacity(64 * 1024);
            while consumed_data < 1024 * 1024 {
                conn.read_buf(&mut buffer).await.unwrap();
                consumed_data += buffer.len();
                buffer.clear();
            }

            let throttled_at_now = loop {
                let res =
                    tokio::time::timeout(Duration::from_secs(2), sent_chunks_rx.changed()).await;
                if res.is_err() {
                    break *sent_chunks_rx.borrow();
                }
            };

            assert!(throttled_at_now > throttled_at);
            drop(sent_chunks_rx);
        };

        tokio::join!(client_fut, peer_fut);
    }

    #[tokio::test]
    async fn throttling_inbound() {
        let mut router = test_router().await;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        router.start_connect(Uid::new_v4(), listener.local_addr().unwrap().into());
        let (mut conn, ..) = listener.accept().await.unwrap();
        let RouterUpdate::ConnectOk { .. } = router.recv().await.unwrap().unwrap() else {
            unreachable!();
        };

        let (sent_bytes_tx, mut sent_bytes_rx) = watch::channel(0);
        let peer_fut = async {
            loop {
                tokio::select! {
                    _ = sent_bytes_tx.closed() => break,
                    res = conn.write(DATA) => {
                        let sent = res.unwrap();
                        sent_bytes_tx.send_modify(|count| *count += sent);
                    },
                }
            }
        };
        let client_fut = async {
            let throttled_at = loop {
                let res =
                    tokio::time::timeout(Duration::from_secs(2), sent_bytes_rx.changed()).await;
                if res.is_err() {
                    break *sent_bytes_rx.borrow();
                }
            };

            let mut consumed_data = 0;
            while consumed_data < 1024 * 1024 {
                match router.recv().await.unwrap().unwrap() {
                    RouterUpdate::ConnEvent {
                        event: ConnEvent::ReadData(data),
                        ..
                    } => {
                        consumed_data += data.len();
                    }
                    other => panic!("unexpected router event {other:?}"),
                }
            }

            let throttled_at_now = loop {
                let res =
                    tokio::time::timeout(Duration::from_secs(2), sent_bytes_rx.changed()).await;
                if res.is_err() {
                    break *sent_bytes_rx.borrow();
                }
            };

            assert!(throttled_at_now > throttled_at);
            drop(sent_bytes_rx);
        };

        tokio::join!(client_fut, peer_fut);
    }
}
