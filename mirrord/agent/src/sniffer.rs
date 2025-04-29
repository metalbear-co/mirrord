use std::{
    collections::{hash_map::Entry, HashMap},
    fmt,
    net::Ipv4Addr,
    ops::Not,
};

use futures::{stream::FuturesUnordered, StreamExt};
use mirrord_protocol::Port;
use pnet::packet::tcp::TcpFlags;
use tcp_capture::TcpCapture;
use tokio::{
    select,
    sync::{
        broadcast,
        mpsc::{error::TrySendError, Receiver, Sender},
    },
};
use tokio_util::sync::CancellationToken;
use tracing::Level;

use self::{
    messages::{SniffedConnection, SnifferCommand, SnifferCommandInner},
    tcp_capture::RawSocketTcpCapture,
};
use crate::{
    error::AgentResult,
    http::HttpVersion,
    metrics::{MIRROR_CONNECTION_SUBSCRIPTION, MIRROR_PORT_SUBSCRIPTION},
    util::{ChannelClosedFuture, ClientId, Subscriptions},
};

pub(crate) mod api;
pub(crate) mod messages;
pub(crate) mod tcp_capture;

/// Identifies one direction of a TCP stream.
///
/// TCP stream are bidirectional. This struct identifies one of the directions.
/// Its [`Eq`] and [`Hash`](std::hash::Hash) implementations
/// make sure that we differentiate between the directions.
#[derive(Debug, Eq, PartialEq, Hash, Copy, Clone)]
pub(crate) struct TcpSessionDirectionId {
    /// The address of the peer that's sending data.
    ///
    /// # Details
    ///
    /// If you were to `curl {impersonated_pod_ip}:{port}`, this would be the address of whoever
    /// is making the request.
    ///
    /// Note that a service mesh would usually intercept the request and resend from
    /// [`Ipv4Addr::LOCALHOST`].
    pub(crate) source_addr: Ipv4Addr,
    /// The address of the peer that's receiving data.
    ///
    /// This should be the address of the interface on which the [`TcpConnectionSniffer`] set up
    /// its raw socket.
    pub(crate) dest_addr: Ipv4Addr,
    /// Port from which the data is sent.
    pub(crate) source_port: u16,
    /// Port on which the data is received.
    pub(crate) dest_port: u16,
}

type TCPSessionMap = HashMap<TcpSessionDirectionId, broadcast::Sender<Vec<u8>>>;

pub(crate) struct TcpPacketData {
    bytes: Vec<u8>,
    flags: u8,
}

impl TcpPacketData {
    fn is_new_connection(&self) -> bool {
        0 != (self.flags & TcpFlags::SYN)
            && 0 == (self.flags & (TcpFlags::ACK | TcpFlags::RST | TcpFlags::FIN))
    }

    fn is_closed_connection(&self) -> bool {
        0 != (self.flags & (TcpFlags::FIN | TcpFlags::RST))
    }

    /// Checks whether this packet starts a new mirrored connection in the sniffer.
    ///
    /// First it checks [`Self::is_new_connection`].
    /// If that's not the case, meaning that the connection existed before the sniffer had started,
    /// then it tries to see if [`Self::bytes`] contains an HTTP request of some sort. When an
    /// HTTP request is detected, then the agent should start mirroring as if it was a new
    /// connection.
    #[tracing::instrument(level = Level::TRACE, ret)]
    fn treat_as_new_session(&self) -> bool {
        self.is_new_connection()
            || matches!(
                HttpVersion::new(&self.bytes),
                Some(HttpVersion::V1 | HttpVersion::V2)
            )
    }
}

impl fmt::Debug for TcpPacketData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TcpPacketData")
            .field("flags", &self.flags)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

/// Main struct implementing incoming traffic mirroring feature.
/// Utilizes [`TcpCapture`] for sniffing on incoming TCP packets. Transforms them into
/// incoming TCP data streams and sends copy of the traffic to all subscribed clients.
///
/// Can be easily used via [`api::TcpSnifferApi`].
///
/// # Notes on behavior under high load
///
/// Because this struct does not talk directly with the remote peers, we can't apply any back
/// pressure on the incoming connections. There is no reliable mechanism to ensure that all
/// subscribed clients receive all of the traffic. If we wait too long when distributing data
/// between the clients, raw socket's recv buffer will overflow and we'll lose packets.
///
/// Having this in mind, this struct distributes incoming data using [`broadcast`] channels. If the
/// clients are not fast enough to pick up TCP packets, they will lose them
/// ([`broadcast::error::RecvError::Lagged`]).
///
/// At the same time, notifying clients about new connections (and distributing
/// [`broadcast::Receiver`]s) is done with [`tokio::sync::mpsc`] channels (one per client).
/// To prevent global packet loss, this struct does not use the blocking [`Sender::send`] method. It
/// uses the non-blocking [`Sender::try_send`] method, so if the client is not fast enough to pick
/// up the [`broadcast::Receiver`], they will miss the whole connection.
pub(crate) struct TcpConnectionSniffer<T> {
    command_rx: Receiver<SnifferCommand>,
    tcp_capture: T,

    port_subscriptions: Subscriptions<Port, ClientId>,
    sessions: TCPSessionMap,

    client_txs: HashMap<ClientId, Sender<SniffedConnection>>,
    clients_closed: FuturesUnordered<ChannelClosedFuture>,
}

impl<T> Drop for TcpConnectionSniffer<T> {
    fn drop(&mut self) {
        MIRROR_PORT_SUBSCRIPTION.store(0, std::sync::atomic::Ordering::Relaxed);
        MIRROR_CONNECTION_SUBSCRIPTION.store(0, std::sync::atomic::Ordering::Relaxed);
    }
}

impl<T> fmt::Debug for TcpConnectionSniffer<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TcpConnectionSniffer")
            .field("clients", &self.client_txs.keys())
            .field("port_subscriptions", &self.port_subscriptions)
            .field("open_tcp_sessions", &self.sessions.keys())
            .finish()
    }
}

impl TcpConnectionSniffer<RawSocketTcpCapture> {
    /// Creates and prepares a new [`TcpConnectionSniffer`] that uses BPF filters to capture network
    /// packets.
    ///
    /// The capture uses a network interface specified by the user, if there is none, then it tries
    /// to find a proper one by starting a connection. If this fails, we use "eth0" as a last
    /// resort.
    #[tracing::instrument(level = Level::TRACE, skip(command_rx), err)]
    pub async fn new(
        command_rx: Receiver<SnifferCommand>,
        network_interface: Option<String>,
        is_mesh: bool,
    ) -> AgentResult<Self> {
        let tcp_capture = RawSocketTcpCapture::new(network_interface, is_mesh).await?;

        Ok(Self {
            command_rx,
            tcp_capture,

            port_subscriptions: Default::default(),
            sessions: TCPSessionMap::new(),

            client_txs: HashMap::new(),
            clients_closed: Default::default(),
        })
    }
}

impl<R> TcpConnectionSniffer<R>
where
    R: TcpCapture,
{
    /// Capacity of [`broadcast`] channels used to distribute incoming TCP packets between clients.
    const CONNECTION_DATA_CHANNEL_CAPACITY: usize = 512;

    /// Runs the sniffer loop, capturing packets.
    #[tracing::instrument(level = Level::DEBUG, skip(cancel_token), err)]
    pub async fn start(mut self, cancel_token: CancellationToken) -> AgentResult<()> {
        loop {
            select! {
                command = self.command_rx.recv() => {
                    let Some(command) = command else {
                        tracing::debug!("command channel closed, exiting");
                        break;
                    };

                    self.handle_command(command)?;
                },

                Some(client_id) = self.clients_closed.next() => {
                    self.handle_client_closed(client_id)?;
                }

                result = self.tcp_capture.next() => {
                    let (identifier, packet_data) = result?;
                    self.handle_packet(identifier, packet_data)?;
                }

                _ = cancel_token.cancelled() => {
                    tracing::debug!("token cancelled, exiting");
                    break;
                }
            }
        }

        Ok(())
    }

    /// New layer is connecting to this agent sniffer.
    #[tracing::instrument(level = Level::TRACE, skip(sender))]
    fn handle_new_client(&mut self, client_id: ClientId, sender: Sender<SniffedConnection>) {
        self.client_txs.insert(client_id, sender.clone());
        self.clients_closed
            .push(ChannelClosedFuture::new(sender.clone(), client_id));
    }

    /// Removes the client with `client_id`, and also unsubscribes its port.
    /// Adjusts BPF filter if needed.
    #[tracing::instrument(level = Level::TRACE, err)]
    fn handle_client_closed(&mut self, client_id: ClientId) -> AgentResult<()> {
        self.client_txs.remove(&client_id);

        if self.port_subscriptions.remove_client(client_id) {
            self.update_packet_filter()?;
        }

        Ok(())
    }

    /// Updates BPF filter used by [`Self::tcp_capture`] to match state of
    /// [`Self::port_subscriptions`].
    #[tracing::instrument(level = Level::TRACE, err)]
    fn update_packet_filter(&mut self) -> AgentResult<()> {
        let ports = self.port_subscriptions.get_subscribed_topics();
        MIRROR_PORT_SUBSCRIPTION.store(ports.len(), std::sync::atomic::Ordering::Relaxed);

        let filter = if ports.is_empty() {
            tracing::trace!("No ports subscribed, setting dummy bpf");
            rawsocket::filter::build_drop_always()
        } else {
            rawsocket::filter::build_tcp_port_filter(&ports)
        };

        self.tcp_capture.set_filter(filter)?;

        Ok(())
    }

    #[tracing::instrument(level = Level::TRACE, err)]
    fn handle_command(&mut self, command: SnifferCommand) -> AgentResult<()> {
        match command {
            SnifferCommand {
                client_id,
                command: SnifferCommandInner::NewClient(sender),
            } => {
                self.handle_new_client(client_id, sender);
            }

            SnifferCommand {
                client_id,
                command: SnifferCommandInner::Subscribe(port, tx),
            } => {
                if self.port_subscriptions.subscribe(client_id, port) {
                    self.update_packet_filter()?;
                }

                let _ = tx.send(port);
            }

            SnifferCommand {
                client_id,
                command: SnifferCommandInner::UnsubscribePort(port),
            } => {
                if self.port_subscriptions.unsubscribe(client_id, port) {
                    self.update_packet_filter()?;
                }
            }
        }

        Ok(())
    }

    /// Handles TCP packet sniffed by [`Self::tcp_capture`].
    #[tracing::instrument(level = Level::TRACE, skip(self), ret, err)]
    fn handle_packet(
        &mut self,
        identifier: TcpSessionDirectionId,
        tcp_packet: TcpPacketData,
    ) -> AgentResult<()> {
        let data_tx = match self.sessions.entry(identifier) {
            Entry::Occupied(e) => e,
            Entry::Vacant(e) => {
                // Performs a check on the `tcp_flags` and on the packet contents to see if this
                // should be treated as a new connection.
                if tcp_packet.treat_as_new_session().not() {
                    // Either it's an existing session, or some sort of existing traffic we don't
                    // care to start mirroring.
                    return Ok(());
                }

                let Some(client_ids) = self
                    .port_subscriptions
                    .get_topic_subscribers(identifier.dest_port)
                    .filter(|ids| !ids.is_empty())
                else {
                    return Ok(());
                };

                tracing::trace!(
                    ?client_ids,
                    "TCP packet should be treated as new session and start connections for clients"
                );

                let (data_tx, _) = broadcast::channel(Self::CONNECTION_DATA_CHANNEL_CAPACITY);

                for client_id in client_ids {
                    let Some(client_tx) = self.client_txs.get(client_id) else {
                        tracing::error!(
                            client_id,
                            destination_port = identifier.dest_port,
                            source_port = identifier.source_port,
                            tcp_flags = tcp_packet.flags,
                            bytes = tcp_packet.bytes.len(),
                            "Failed to find client while handling new sniffed TCP connection, this is a bug",
                        );

                        continue;
                    };

                    let connection = SniffedConnection {
                        session_id: identifier,
                        data: data_tx.subscribe(),
                    };

                    match client_tx.try_send(connection) {
                        Ok(()) => {}

                        Err(TrySendError::Closed(..)) => {
                            // Client closed.
                            // State will be cleaned up when `self.clients_closed` picks it up.
                        }

                        Err(TrySendError::Full(..)) => {
                            tracing::warn!(
                                client_id,
                                destination_port = identifier.dest_port,
                                source_port = identifier.source_port,
                                tcp_flags = tcp_packet.flags,
                                bytes = tcp_packet.bytes.len(),
                                "Client queue of new sniffed TCP connections is full, dropping",
                            );

                            continue;
                        }
                    }
                }

                MIRROR_CONNECTION_SUBSCRIPTION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                e.insert_entry(data_tx)
            }
        };

        tracing::trace!("Resolved data broadcast channel");

        let closes_connection = tcp_packet.is_closed_connection();

        if !tcp_packet.bytes.is_empty() && data_tx.get().send(tcp_packet.bytes).is_err() {
            tracing::trace!("All data receivers are dead, dropping data broadcast sender");
            data_tx.remove();
            return Ok(());
        }

        if closes_connection {
            tracing::trace!("TCP packet closes connection, dropping data broadcast channel");
            data_tx.remove();
        }

        Ok(())
    }
}

#[cfg(test)]
mod test {
    use std::{
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
        time::{Duration, Instant},
    };

    use api::TcpSnifferApi;
    use mirrord_protocol::{
        tcp::{DaemonTcp, LayerTcp, NewTcpConnectionV1, TcpClose, TcpData},
        ConnectionId, LogLevel,
    };
    use rstest::rstest;
    use tcp_capture::test::TcpPacketsChannel;
    use tokio::sync::mpsc;

    use super::*;
    use crate::util::remote_runtime::{BgTaskRuntime, BgTaskStatus, IntoStatus};

    struct TestSnifferSetup {
        command_tx: Sender<SnifferCommand>,
        task_status: BgTaskStatus,
        packet_tx: Sender<(TcpSessionDirectionId, TcpPacketData)>,
        times_filter_changed: Arc<AtomicUsize>,
        next_client_id: ClientId,
    }

    impl TestSnifferSetup {
        async fn get_api(&mut self) -> TcpSnifferApi {
            let client_id = self.next_client_id;
            self.next_client_id += 1;

            TcpSnifferApi::new(client_id, self.command_tx.clone(), self.task_status.clone())
                .await
                .unwrap()
        }

        fn times_filter_changed(&self) -> usize {
            self.times_filter_changed.load(Ordering::Relaxed)
        }

        fn new() -> Self {
            let (packet_tx, packet_rx) = mpsc::channel(128);
            let (command_tx, command_rx) = mpsc::channel(16);
            let times_filter_changed = Arc::new(AtomicUsize::default());

            let sniffer = TcpConnectionSniffer {
                command_rx,
                tcp_capture: TcpPacketsChannel {
                    times_filter_changed: times_filter_changed.clone(),
                    receiver: packet_rx,
                },
                port_subscriptions: Default::default(),
                sessions: Default::default(),
                client_txs: Default::default(),
                clients_closed: Default::default(),
            };

            let task_status = BgTaskRuntime::Local
                .spawn(sniffer.start(CancellationToken::new()))
                .into_status("TcpSnifferTask");

            Self {
                command_tx,
                task_status,
                packet_tx,
                times_filter_changed,
                next_client_id: 0,
            }
        }
    }

    /// Simulates two sniffed connections, only one matching client's subscription.
    #[tokio::test]
    async fn one_client() {
        let mut setup = TestSnifferSetup::new();
        let mut api = setup.get_api().await;

        api.handle_client_message(LayerTcp::PortSubscribe(80))
            .await
            .unwrap();

        assert_eq!(
            api.recv().await.unwrap(),
            (DaemonTcp::SubscribeResult(Ok(80)), None),
        );

        for dest_port in [80, 81] {
            setup
                .packet_tx
                .send((
                    TcpSessionDirectionId {
                        source_addr: "1.1.1.1".parse().unwrap(),
                        dest_addr: "127.0.0.1".parse().unwrap(),
                        source_port: 3133,
                        dest_port,
                    },
                    TcpPacketData {
                        bytes: b"hello_1".into(),
                        flags: TcpFlags::SYN,
                    },
                ))
                .await
                .unwrap();

            setup
                .packet_tx
                .send((
                    TcpSessionDirectionId {
                        source_addr: "1.1.1.1".parse().unwrap(),
                        dest_addr: "127.0.0.1".parse().unwrap(),
                        source_port: 3133,
                        dest_port: 80,
                    },
                    TcpPacketData {
                        bytes: b"hello_2".into(),
                        flags: TcpFlags::FIN,
                    },
                ))
                .await
                .unwrap();
        }

        let (message, log) = api.recv().await.unwrap();
        assert_eq!(
            message,
            DaemonTcp::NewConnectionV1(NewTcpConnectionV1 {
                connection_id: 0,
                remote_address: "1.1.1.1".parse().unwrap(),
                destination_port: 80,
                source_port: 3133,
                local_address: "127.0.0.1".parse().unwrap(),
            }),
        );
        assert_eq!(log, None);

        let (message, log) = api.recv().await.unwrap();
        assert_eq!(
            message,
            DaemonTcp::Data(TcpData {
                connection_id: 0,
                bytes: b"hello_1".into(),
            }),
        );
        assert_eq!(log, None);

        let (message, log) = api.recv().await.unwrap();
        assert_eq!(
            message,
            DaemonTcp::Data(TcpData {
                connection_id: 0,
                bytes: b"hello_2".into(),
            }),
        );
        assert_eq!(log, None);

        let (message, log) = api.recv().await.unwrap();
        assert_eq!(message, DaemonTcp::Close(TcpClose { connection_id: 0 }),);
        assert_eq!(log, None);
    }

    /// Tests that [`TcpCapture`] filter is replaced only when needed.
    ///
    /// # Note
    ///
    /// Due to fact that [`LayerTcp::PortUnsubscribe`] request does not generate any response, this
    /// test does some sleeping to give the sniffer time to process.
    #[tokio::test]
    async fn filter_replace() {
        let mut setup = TestSnifferSetup::new();

        let mut api_1 = setup.get_api().await;
        let mut api_2 = setup.get_api().await;

        api_1
            .handle_client_message(LayerTcp::PortSubscribe(80))
            .await
            .unwrap();
        assert_eq!(
            api_1.recv().await.unwrap(),
            (DaemonTcp::SubscribeResult(Ok(80)), None),
        );
        assert_eq!(setup.times_filter_changed(), 1);

        api_2
            .handle_client_message(LayerTcp::PortSubscribe(80))
            .await
            .unwrap();
        assert_eq!(
            api_2.recv().await.unwrap(),
            (DaemonTcp::SubscribeResult(Ok(80)), None),
        );
        assert_eq!(setup.times_filter_changed(), 1); // api_1 already subscribed `80`

        api_2
            .handle_client_message(LayerTcp::PortSubscribe(81))
            .await
            .unwrap();
        assert_eq!(
            api_2.recv().await.unwrap(),
            (DaemonTcp::SubscribeResult(Ok(81)), None),
        );
        assert_eq!(setup.times_filter_changed(), 2);

        api_1
            .handle_client_message(LayerTcp::PortSubscribe(81))
            .await
            .unwrap();
        assert_eq!(
            api_1.recv().await.unwrap(),
            (DaemonTcp::SubscribeResult(Ok(81)), None),
        );
        assert_eq!(setup.times_filter_changed(), 2); // api_2 already subscribed `81`

        api_1
            .handle_client_message(LayerTcp::PortUnsubscribe(80))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(setup.times_filter_changed(), 2); // api_2 still subscribes `80`

        api_2
            .handle_client_message(LayerTcp::PortUnsubscribe(81))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(setup.times_filter_changed(), 2); // api_1 still subscribes `81`

        api_1
            .handle_client_message(LayerTcp::PortUnsubscribe(81))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(setup.times_filter_changed(), 3);

        api_2
            .handle_client_message(LayerTcp::PortUnsubscribe(80))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(setup.times_filter_changed(), 4);
    }

    /// Simulates scenario where client does not read connection data fast enough.
    /// Packet buffer should overflow in the [`broadcast`] channel and the client should see the
    /// connection being closed.
    #[tokio::test]
    async fn client_lagging_on_data() {
        let mut setup = TestSnifferSetup::new();
        let mut api = setup.get_api().await;

        api.handle_client_message(LayerTcp::PortSubscribe(80))
            .await
            .unwrap();

        assert_eq!(
            api.recv().await.unwrap(),
            (DaemonTcp::SubscribeResult(Ok(80)), None),
        );

        let session_id = TcpSessionDirectionId {
            source_addr: "1.1.1.1".parse().unwrap(),
            dest_addr: "127.0.0.1".parse().unwrap(),
            source_port: 3133,
            dest_port: 80,
        };

        setup
            .packet_tx
            .send((
                session_id,
                TcpPacketData {
                    bytes: b"hello".into(),
                    flags: TcpFlags::SYN,
                },
            ))
            .await
            .unwrap();

        let (message, log) = api.recv().await.unwrap();
        assert_eq!(
            message,
            DaemonTcp::NewConnectionV1(NewTcpConnectionV1 {
                connection_id: 0,
                remote_address: session_id.source_addr.into(),
                destination_port: session_id.dest_port,
                source_port: session_id.source_port,
                local_address: session_id.dest_addr.into(),
            }),
        );
        assert_eq!(log, None);

        let (message, log) = api.recv().await.unwrap();
        assert_eq!(
            message,
            DaemonTcp::Data(TcpData {
                connection_id: 0,
                bytes: b"hello".to_vec(),
            }),
        );
        assert_eq!(log, None);

        for _ in 0..TcpConnectionSniffer::<TcpPacketsChannel>::CONNECTION_DATA_CHANNEL_CAPACITY + 2
        {
            setup
                .packet_tx
                .send((
                    session_id,
                    TcpPacketData {
                        bytes: vec![0],
                        flags: 0,
                    },
                ))
                .await
                .unwrap();
        }

        // Wait until sniffer consumes all messages.
        setup
            .packet_tx
            .reserve_many(setup.packet_tx.max_capacity())
            .await
            .unwrap();

        let (message, log) = api.recv().await.unwrap();
        assert_eq!(message, DaemonTcp::Close(TcpClose { connection_id: 0 }),);
        let log = log.unwrap();
        assert_eq!(log.level, LogLevel::Error);
    }

    /// Simulates scenario where client does not read notifications about new connections fast
    /// enough. Client should miss new connections.
    #[tokio::test]
    async fn client_lagging_on_new_connections() {
        let mut setup = TestSnifferSetup::new();
        let mut api = setup.get_api().await;

        api.handle_client_message(LayerTcp::PortSubscribe(80))
            .await
            .unwrap();

        assert_eq!(
            api.recv().await.unwrap(),
            (DaemonTcp::SubscribeResult(Ok(80)), None),
        );

        let source_addr = "1.1.1.1".parse().unwrap();
        let dest_addr = "127.0.0.1".parse().unwrap();

        // First send `TcpSnifferApi::CONNECTION_CHANNEL_SIZE` + 2 first connections.
        let session_ids =
            (0..=TcpSnifferApi::CONNECTION_CHANNEL_SIZE).map(|idx| TcpSessionDirectionId {
                source_addr,
                dest_addr,
                source_port: 3000 + idx as u16,
                dest_port: 80,
            });
        for session in session_ids {
            setup
                .packet_tx
                .send((
                    session,
                    TcpPacketData {
                        bytes: Default::default(),
                        flags: TcpFlags::SYN,
                    },
                ))
                .await
                .unwrap();
        }

        // Wait until sniffer processes all packets.
        let permit = setup
            .packet_tx
            .reserve_many(setup.packet_tx.max_capacity())
            .await
            .unwrap();
        std::mem::drop(permit);

        // Verify that we picked up `TcpSnifferApi::CONNECTION_CHANNEL_SIZE` first connections.
        for i in 0..TcpSnifferApi::CONNECTION_CHANNEL_SIZE {
            let (msg, log) = api.recv().await.unwrap();
            assert_eq!(log, None);
            assert_eq!(
                msg,
                DaemonTcp::NewConnectionV1(NewTcpConnectionV1 {
                    connection_id: i as ConnectionId,
                    remote_address: source_addr.into(),
                    destination_port: 80,
                    source_port: 3000 + i as u16,
                    local_address: dest_addr.into(),
                })
            )
        }

        // Send one more connection.
        setup
            .packet_tx
            .send((
                TcpSessionDirectionId {
                    source_addr,
                    dest_addr,
                    source_port: 3222,
                    dest_port: 80,
                },
                TcpPacketData {
                    bytes: Default::default(),
                    flags: TcpFlags::SYN,
                },
            ))
            .await
            .unwrap();

        // Verify that we missed the last connections from the first batch.
        let (msg, log) = api.recv().await.unwrap();
        assert_eq!(log, None);
        assert_eq!(
            msg,
            DaemonTcp::NewConnectionV1(NewTcpConnectionV1 {
                connection_id: TcpSnifferApi::CONNECTION_CHANNEL_SIZE as ConnectionId,
                remote_address: source_addr.into(),
                destination_port: 80,
                source_port: 3222,
                local_address: dest_addr.into(),
            }),
        );
    }

    /// Verifies that [`TcpConnectionSniffer`] reacts to [`TcpSnifferApi`] being dropped
    /// and clears the packet filter.
    #[rstest]
    #[timeout(Duration::from_secs(5))]
    #[tokio::test]
    async fn cleanup_on_client_closed() {
        let mut setup = TestSnifferSetup::new();

        let mut api = setup.get_api().await;

        api.handle_client_message(LayerTcp::PortSubscribe(80))
            .await
            .unwrap();
        assert_eq!(
            api.recv().await.unwrap(),
            (DaemonTcp::SubscribeResult(Ok(80)), None),
        );
        assert_eq!(setup.times_filter_changed(), 1);

        std::mem::drop(api);
        let dropped_at = Instant::now();

        loop {
            match setup.times_filter_changed() {
                1 => {
                    println!(
                        "filter still not changed {}ms after client closed",
                        dropped_at.elapsed().as_millis()
                    );
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }

                2 => {
                    break;
                }

                other => panic!("unexpected times filter changed {other}"),
            }
        }
    }
}
