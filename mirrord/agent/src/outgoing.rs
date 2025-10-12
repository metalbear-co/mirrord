//! Implementation of the outgoing traffic feature.

use std::{
    collections::{HashMap, HashSet, VecDeque, hash_map::Entry},
    fmt, io,
    ops::{ControlFlow, Not, RangeInclusive},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use futures::{Stream, StreamExt, stream::FuturesUnordered};
use mirrord_protocol::{
    DaemonMessage, LogMessage, Payload, RemoteError, ResponseError,
    outgoing::{
        DaemonConnect, DaemonRead, LayerClose, LayerConnect, LayerWrite, SocketAddress,
        tcp::LayerTcpOutgoing, udp::LayerUdpOutgoing, v2,
    },
    uid::Uid,
};
use tokio::{
    sync::mpsc::{self, Receiver, Sender},
    time,
};
use tokio_stream::{StreamMap, StreamNotifyClose};

use crate::{
    error::{AgentError, AgentResult},
    outgoing::socket::{ReadHalf, WriteHalf},
    task::{
        BgTaskRuntime,
        status::{BgTaskStatus, IntoStatus},
    },
    util::path_resolver::InTargetPathResolver,
};

mod socket;

/// [`mirrord_protocol`] messages consumed by the [`OutgoingApi`].
pub(crate) enum OutgoingApiMessage {
    UdpV1(LayerUdpOutgoing),
    TcpV1(LayerTcpOutgoing),
    V2(v2::ClientOutgoing),
}

/// Opaque handle to a background task driving IO of the outgoing connections for a **single** agent
/// client.
pub(crate) struct OutgoingApi {
    task_status: BgTaskStatus,
    tx: Sender<OutgoingApiMessage>,
    rx: Receiver<DaemonMessage>,
}

impl OutgoingApi {
    /// Creates a new instance backed by a dedicated background task.
    pub(crate) fn new(runtime: &BgTaskRuntime) -> Self {
        // IMPORTANT: this makes tokio tasks spawn on `runtime`.
        // Do not remove this.
        let _rt = runtime.handle().enter();

        let (layer_tx, layer_rx) = mpsc::channel(128);
        let (daemon_tx, daemon_rx) = mpsc::channel(128);

        let pid = runtime.target_pid();
        let task_status = tokio::spawn(OutgoingTask::new(pid, layer_rx, daemon_tx).run())
            .into_status("OutgoingTask");

        Self {
            task_status,
            tx: layer_tx,
            rx: daemon_rx,
        }
    }

    pub(crate) async fn recv(&mut self) -> AgentResult<DaemonMessage> {
        match self.rx.recv().await {
            Some(message) => Ok(message),
            None => Err(self.task_status.wait_assert_running().await),
        }
    }

    pub(crate) async fn send(&self, message: OutgoingApiMessage) -> AgentResult<()> {
        if self.tx.send(message).await.is_ok() {
            Ok(())
        } else {
            Err(self.task_status.wait_assert_running().await)
        }
    }
}

/// Compatibility wrapper for v1 and v2 outgoing connection ids.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum ConnectionId {
    V1(mirrord_protocol::ConnectionId, v2::OutgoingProtocol),
    V2(Uid),
}

impl fmt::Debug for ConnectionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::V1(id, proto) => write!(f, "{id}-{proto}"),
            Self::V2(id) => id.fmt(f),
        }
    }
}

impl fmt::Display for ConnectionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::V1(id, proto) => write!(f, "{id}-{proto}"),
            Self::V2(id) => id.fmt(f),
        }
    }
}

/// Background tasks that drives IO for the [`OutgoingApi`].
struct OutgoingTask {
    /// Used to resolve paths to unix sockets in the target container.
    ///
    /// [`None`] if there is no target container.
    path_resolver: Option<Arc<InTargetPathResolver>>,
    /// For receiving messages from the [`OutgoingApi`].
    rx: Receiver<OutgoingApiMessage>,
    /// For sending messages to the [`OutgoingApi`].
    tx: Sender<DaemonMessage>,

    /// Pool of v1 connection ids.
    connection_ids_v1: RangeInclusive<mirrord_protocol::ConnectionId>,

    /// In progress v1 connect requests.
    ///
    /// These requests are processed **strictly sequentially**,
    /// and [`ConnectFut`]s are progressed one at a time. Rationale:
    ///
    /// 1. The order of responses sent back to the client matters.
    /// 2. We don't want to make connections concurrently, because head of line blocking could make
    ///    other connections sit idle, and possibly be broken by the peers due to inactivity. Thus,
    ///    we don't use [`futures::stream::FuturesOrdered`].
    connects_v1: ConnectQueue,
    /// In progress v2 connect requests.
    ///
    /// Requests are processed concurrently.
    connects_v2: FuturesUnordered<ConnectFut>,
    /// Ids of in progress v2 connect requests.
    connects_v2_ids: HashSet<Uid>,

    /// Writing halves of open connections.
    writers: HashMap<ConnectionId, WriteHalf>,
    /// Reading halves of open connections.
    readers: StreamMap<ConnectionId, StreamNotifyClose<ReadHalf>>,
}

impl OutgoingTask {
    /// Hard timeout for connect attempts.
    const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

    fn new(pid: Option<u64>, rx: Receiver<OutgoingApiMessage>, tx: Sender<DaemonMessage>) -> Self {
        Self {
            path_resolver: pid.map(InTargetPathResolver::new).map(Arc::new),
            rx,
            tx,
            connection_ids_v1: (mirrord_protocol::ConnectionId::MIN
                ..=mirrord_protocol::ConnectionId::MAX),
            connects_v1: Default::default(),
            connects_v2: Default::default(),
            connects_v2_ids: Default::default(),
            writers: Default::default(),
            readers: Default::default(),
        }
    }

    async fn run(mut self) -> Result<(), AgentError> {
        loop {
            let result = tokio::select! {
                message = self.rx.recv() => match message {
                    Some(OutgoingApiMessage::TcpV1(msg)) => self.handle_tcp_v1_message(msg).await.map_break(Ok),
                    Some(OutgoingApiMessage::UdpV1(msg)) => self.handle_udp_v1_message(msg).await.map_break(Ok),
                    Some(OutgoingApiMessage::V2(msg)) => self.handle_v2_message(msg).await,
                    None => ControlFlow::Break(Ok(()))
                },

                Some(result) = self.connects_v1.next() => {
                    self.handle_connect_result(result).await
                }

                Some(result) = self.connects_v2.next() => {
                    self.handle_connect_result(result).await
                }

                Some((id, result)) = self.readers.next() => {
                    self.handle_read(id, result).await.map_break(Ok)
                }
            };

            if let Some(result) = result.break_value() {
                break result;
            }
        }
    }

    async fn handle_tcp_v1_message(&mut self, message: LayerTcpOutgoing) -> ControlFlow<()> {
        match message {
            LayerTcpOutgoing::Connect(LayerConnect { remote_address }) => {
                let fut = Box::pin(Self::connect(
                    remote_address,
                    v2::OutgoingProtocol::Tcp,
                    self.path_resolver.clone(),
                    None,
                ));
                self.connects_v1.push(fut);
                ControlFlow::Continue(())
            }

            LayerTcpOutgoing::Write(LayerWrite {
                connection_id,
                bytes,
            }) => {
                let id = ConnectionId::V1(connection_id, v2::OutgoingProtocol::Tcp);
                self.handle_write(id, bytes).await
            }

            LayerTcpOutgoing::Close(LayerClose { connection_id }) => {
                let connection_id = ConnectionId::V1(connection_id, v2::OutgoingProtocol::Tcp);
                self.readers.remove(&connection_id);
                self.writers.remove(&connection_id);
                ControlFlow::Continue(())
            }
        }
    }

    async fn handle_udp_v1_message(&mut self, message: LayerUdpOutgoing) -> ControlFlow<()> {
        match message {
            LayerUdpOutgoing::Connect(LayerConnect { remote_address }) => {
                let fut = Box::pin(Self::connect(
                    remote_address,
                    v2::OutgoingProtocol::Udp,
                    self.path_resolver.clone(),
                    None,
                ));
                self.connects_v1.push(fut);
                ControlFlow::Continue(())
            }

            LayerUdpOutgoing::Write(LayerWrite {
                connection_id,
                bytes,
            }) => {
                let id = ConnectionId::V1(connection_id, v2::OutgoingProtocol::Udp);
                self.handle_write(id, bytes).await
            }

            LayerUdpOutgoing::Close(LayerClose { connection_id }) => {
                let connection_id = ConnectionId::V1(connection_id, v2::OutgoingProtocol::Udp);
                self.readers.remove(&connection_id);
                self.writers.remove(&connection_id);
                ControlFlow::Continue(())
            }
        }
    }

    async fn handle_v2_message(
        &mut self,
        message: v2::ClientOutgoing,
    ) -> ControlFlow<AgentResult<()>> {
        match message {
            v2::ClientOutgoing::Connect(connect) => {
                if self.connects_v2_ids.insert(connect.id).not()
                    || self.readers.contains_key(&ConnectionId::V2(connect.id))
                    || self.writers.contains_key(&ConnectionId::V2(connect.id))
                {
                    return ControlFlow::Break(Err(AgentError::DuplicateOutgoingConnectionId(
                        connect.id,
                    )));
                }
                let fut = Box::pin(Self::connect(
                    connect.address,
                    connect.protocol,
                    self.path_resolver.clone(),
                    Some(connect.id),
                ));
                self.connects_v2.push(fut);
                ControlFlow::Continue(())
            }
            v2::ClientOutgoing::Data(data) => self
                .handle_write(ConnectionId::V2(data.id), data.data)
                .await
                .map_break(Ok),
            v2::ClientOutgoing::Close(close) => {
                let connection_id = ConnectionId::V2(close.id);
                self.readers.remove(&connection_id);
                self.writers.remove(&connection_id);
                ControlFlow::Continue(())
            }
        }
    }

    async fn handle_write(
        &mut self,
        connection_id: ConnectionId,
        bytes: Payload,
    ) -> ControlFlow<()> {
        match self.writers.entry(connection_id) {
            // Empty write means partial shutdown.
            Entry::Occupied(e) if bytes.is_empty() => {
                e.remove();
                if self.readers.contains_key(&connection_id) {
                    ControlFlow::Continue(())
                } else {
                    let message = match connection_id {
                        ConnectionId::V1(id, proto) => proto.v1_daemon_close(id),
                        ConnectionId::V2(id) => DaemonMessage::OutgoingV2(
                            v2::DaemonOutgoing::Close(v2::OutgoingClose { id }),
                        ),
                    };
                    self.send(message).await
                }
            }

            Entry::Occupied(mut e) => match e.get_mut().write(&bytes).await {
                Ok(()) => ControlFlow::Continue(()),
                Err(error) => {
                    e.remove();
                    self.readers.remove(&connection_id);
                    match connection_id {
                        ConnectionId::V1(id, proto) => {
                            let log = DaemonMessage::LogMessage(LogMessage::warn(format!(
                                "write to outgoing {proto} connection {id} failed: {error}"
                            )));
                            self.send(log).await?;
                            let close = proto.v1_daemon_close(id);
                            self.send(close).await
                        }
                        ConnectionId::V2(id) => {
                            let error = DaemonMessage::OutgoingV2(v2::DaemonOutgoing::Error(
                                v2::OutgoingError {
                                    id,
                                    error: error.into(),
                                },
                            ));
                            self.send(error).await
                        }
                    }
                }
            },

            Entry::Vacant(..) => ControlFlow::Continue(()),
        }
    }

    async fn handle_read(
        &mut self,
        connection_id: ConnectionId,
        result: Option<io::Result<Vec<u8>>>,
    ) -> ControlFlow<()> {
        match result {
            Some(Ok(data)) => {
                let message = match connection_id {
                    ConnectionId::V1(connection_id, proto) => proto.v1_daemon_read(DaemonRead {
                        connection_id,
                        bytes: data.into(),
                    }),
                    ConnectionId::V2(id) => {
                        DaemonMessage::OutgoingV2(v2::DaemonOutgoing::Data(v2::OutgoingData {
                            id,
                            data: data.into(),
                        }))
                    }
                };
                self.send(message).await
            }

            Some(Err(error)) => {
                self.writers.remove(&connection_id);
                match connection_id {
                    ConnectionId::V1(connection_id, proto) => {
                        let log = DaemonMessage::LogMessage(LogMessage::warn(format!(
                            "read from outgoing {proto} connection {connection_id} failed: {error}"
                        )));
                        self.send(log).await?;
                        let close = proto.v1_daemon_close(connection_id);
                        self.send(close).await
                    }
                    ConnectionId::V2(id) => {
                        let error = DaemonMessage::OutgoingV2(v2::DaemonOutgoing::Error(
                            v2::OutgoingError {
                                id,
                                error: error.into(),
                            },
                        ));
                        self.send(error).await
                    }
                }
            }

            None if self.writers.contains_key(&connection_id) => match connection_id {
                ConnectionId::V1(connection_id, proto) => {
                    let read = proto.v1_daemon_read(DaemonRead {
                        connection_id,
                        bytes: Default::default(),
                    });
                    self.send(read).await
                }
                ConnectionId::V2(id) => {
                    let read =
                        DaemonMessage::OutgoingV2(v2::DaemonOutgoing::Data(v2::OutgoingData {
                            id,
                            data: Default::default(),
                        }));
                    self.send(read).await
                }
            },

            None => match connection_id {
                ConnectionId::V1(connection_id, proto) => {
                    let close = proto.v1_daemon_close(connection_id);
                    self.send(close).await
                }
                ConnectionId::V2(id) => {
                    let close =
                        DaemonMessage::OutgoingV2(v2::DaemonOutgoing::Close(v2::OutgoingClose {
                            id,
                        }));
                    self.send(close).await
                }
            },
        }
    }

    async fn send(&self, message: DaemonMessage) -> ControlFlow<()> {
        if self.tx.send(message).await.is_ok() {
            ControlFlow::Continue(())
        } else {
            ControlFlow::Break(())
        }
    }

    async fn connect(
        address: SocketAddress,
        protocol: v2::OutgoingProtocol,
        path_resolver: Option<Arc<InTargetPathResolver>>,
        connection_id: Option<Uid>,
    ) -> Result<ConnectOk, ConnectErr> {
        time::timeout(
            Self::CONNECT_TIMEOUT,
            socket::connect(&address, protocol, path_resolver.as_deref()),
        )
        .await
        .unwrap_or_else(|_elapsed| Err(RemoteError::ConnectTimedOut(address.clone()).into()))
        .and_then(|(read, write)| {
            let local_address = write.local_address()?;
            let peer_address = write.peer_address()?;
            Ok(ConnectOk {
                connection_id,
                protocol,
                write,
                read,
                local_address,
                peer_address,
            })
        })
        .map_err(|error| ConnectErr {
            connection_id,
            protocol,
            error,
        })
    }

    async fn handle_connect_result(
        &mut self,
        result: Result<ConnectOk, ConnectErr>,
    ) -> ControlFlow<AgentResult<()>> {
        match result {
            Ok(ConnectOk {
                connection_id: Some(id),
                read,
                write,
                local_address,
                peer_address,
                ..
            }) => {
                self.connects_v2_ids.remove(&id);
                let message = DaemonMessage::OutgoingV2(v2::DaemonOutgoing::Connect(
                    v2::OutgoingConnectResponse {
                        id,
                        agent_local_address: local_address,
                        agent_peer_address: peer_address,
                    },
                ));
                self.send(message).await.map_break(Ok)?;
                self.writers.insert(ConnectionId::V2(id), write);
                self.readers
                    .insert(ConnectionId::V2(id), StreamNotifyClose::new(read));
                ControlFlow::Continue(())
            }

            Ok(ConnectOk {
                connection_id: None,
                protocol,
                read,
                write,
                local_address,
                peer_address,
            }) => {
                let Some(connection_id) = self.connection_ids_v1.next() else {
                    return ControlFlow::Break(Err(AgentError::ExhaustedConnectionId));
                };
                let message = Ok(DaemonConnect {
                    connection_id,
                    remote_address: peer_address,
                    local_address,
                });
                let message = protocol.v1_daemon_connect(message);
                self.send(message).await.map_break(Ok)?;
                self.writers
                    .insert(ConnectionId::V1(connection_id, protocol), write);
                self.readers.insert(
                    ConnectionId::V1(connection_id, protocol),
                    StreamNotifyClose::new(read),
                );
                ControlFlow::Continue(())
            }

            Err(ConnectErr {
                connection_id: Some(id),
                error,
                ..
            }) => {
                self.connects_v2_ids.remove(&id);
                let message =
                    DaemonMessage::OutgoingV2(v2::DaemonOutgoing::Error(v2::OutgoingError {
                        id,
                        error,
                    }));
                self.send(message).await.map_break(Ok)
            }

            Err(ConnectErr {
                connection_id: None,
                protocol,
                error,
            }) => {
                let message = protocol.v1_daemon_connect(Err(error));
                self.send(message).await.map_break(Ok)
            }
        }
    }
}

struct ConnectOk {
    connection_id: Option<Uid>,
    protocol: v2::OutgoingProtocol,
    write: WriteHalf,
    read: ReadHalf,
    local_address: SocketAddress,
    peer_address: SocketAddress,
}

struct ConnectErr {
    connection_id: Option<Uid>,
    protocol: v2::OutgoingProtocol,
    error: ResponseError,
}

type ConnectFut = Pin<Box<dyn Future<Output = Result<ConnectOk, ConnectErr>> + Send + Sync>>;

#[derive(Default)]
struct ConnectQueue {
    inner: VecDeque<ConnectFut>,
}

impl ConnectQueue {
    fn push(&mut self, fut: ConnectFut) {
        self.inner.push_back(fut);
    }
}

impl Stream for ConnectQueue {
    type Item = Result<ConnectOk, ConnectErr>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let Some(first) = this.inner.front_mut() else {
            return Poll::Ready(None);
        };
        let result = std::task::ready!(Pin::new(first).poll(cx));

        this.inner.pop_front();
        if this.inner.len() < this.inner.capacity() / 2 {
            this.inner.shrink_to_fit();
        }

        Poll::Ready(Some(result))
    }
}
