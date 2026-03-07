use std::{
    collections::{HashMap, HashSet, VecDeque},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    ops::Not,
    time::{Duration, Instant},
};

use futures::StreamExt;
use mirrord_config::feature::network::incoming::IncomingConfig;
use mirrord_intproxy::{
    background_tasks::{BackgroundTasks, TaskError, TaskSender, TaskUpdate},
    main_tasks::{ProxyMessage, ToLayer},
    proxies::incoming::{IncomingProxy, IncomingProxyError, IncomingProxyMessage},
};
use mirrord_intproxy_protocol::{
    IncomingRequest, IncomingResponse, LayerId, PortSubscribe, PortSubscription,
    ProxyToLayerMessage,
};
use mirrord_protocol::{
    CLIENT_READY_FOR_LOGS, ClientMessage, ConnectionId, DaemonMessage, LogLevel, Payload, Port,
    dns::{DnsLookup, GetAddrInfoRequest, GetAddrInfoResponse, LookupRecord},
    outgoing::{
        LayerClose, LayerConnect, LayerWrite, SocketAddress,
        tcp::{DaemonTcpOutgoing, LayerTcpOutgoing},
    },
    tcp::{HttpFilter, MIRROR_HTTP_FILTER_VERSION, MirrorType, StealType},
};
use mirrord_protocol_io::{Client, Connection};
use semver::Version;
use thiserror::Error;
use tokio::{
    io::AsyncWriteExt,
    net::{
        TcpListener, TcpStream,
        tcp::{OwnedReadHalf, OwnedWriteHalf},
    },
    select,
    sync::{
        mpsc::{self, Receiver, Sender},
        oneshot,
    },
};
use tokio_stream::{StreamMap, wrappers::TcpListenerStream};
use tokio_util::io::ReaderStream;
use tracing::Level;

use crate::{AddrPortMapping, LocalPort, RemoteAddr, RemotePort};

/// Connection address pair
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ConnectionSocketPair {
    /// Local addr for connection
    local: SocketAddr,
    /// Peer addr from connection to local addr
    peer: SocketAddr,
}

#[derive(Clone, Debug, PartialEq)]
struct ConnectionPortMapping {
    pair: ConnectionSocketPair,
    remote: SocketAddr,
}

pub struct PortForwarder {
    /// communicates with the agent (only TCP supported)
    agent_connection: Connection<Client>,
    /// associates local ports with destination ports
    /// destinations may contain unresolved hostnames
    raw_mappings: HashMap<SocketAddr, (RemoteAddr, u16)>,
    /// accepts connections from the user app in the form of a stream
    listeners: StreamMap<SocketAddr, TcpListenerStream>,
    /// oneshot channels for sending connection IDs to tasks and the associated address pair
    id_oneshots: VecDeque<(ConnectionSocketPair, oneshot::Sender<ConnectionId>)>,
    /// oneshot channels for sending resolved hostnames to tasks and the associated address pair
    dns_oneshots: VecDeque<(ConnectionSocketPair, oneshot::Sender<IpAddr>)>,
    /// identifies a pair of mapped socket addresses by their corresponding connection ID
    sockets: HashMap<ConnectionId, ConnectionPortMapping>,
    /// identifies task senders by their corresponding address pairs for sending data from the
    /// remote socket to the local address
    task_txs: HashMap<ConnectionSocketPair, Sender<Vec<u8>>>,

    /// transmit internal messages from tasks to [`PortForwarder`]'s main loop.
    internal_msg_tx: Sender<PortForwardMessage>,
    internal_msg_rx: Receiver<PortForwardMessage>,

    /// true if Ping has been sent to agent
    waiting_for_pong: bool,
    ping_pong_timeout: Instant,
}

impl PortForwarder {
    pub(crate) async fn new(
        agent_connection: Connection<Client>,
        mappings: HashMap<SocketAddr, (RemoteAddr, u16)>,
    ) -> Result<Self, PortForwardError> {
        // open tcp listener for local addrs
        let mut listeners = StreamMap::with_capacity(mappings.len());

        for &local_socket in mappings.keys() {
            let listener = TcpListener::bind(local_socket).await;
            match listener {
                Ok(listener) => {
                    listeners.insert(local_socket, TcpListenerStream::new(listener));
                }
                Err(error) => return Err(PortForwardError::TcpListenerError(error)),
            }
        }

        let (internal_msg_tx, internal_msg_rx) = mpsc::channel(1024);

        Ok(Self {
            agent_connection,
            raw_mappings: mappings,
            listeners,
            id_oneshots: VecDeque::new(),
            dns_oneshots: VecDeque::new(),
            sockets: HashMap::new(),
            task_txs: HashMap::new(),
            internal_msg_tx,
            internal_msg_rx,
            waiting_for_pong: false,
            ping_pong_timeout: Instant::now(),
        })
    }

    pub(crate) async fn run(&mut self) -> Result<(), PortForwardError> {
        // setup agent connection
        self.agent_connection
            .send(ClientMessage::SwitchProtocolVersion(
                mirrord_protocol::VERSION.clone(),
            ))
            .await;
        match self.agent_connection.recv().await {
            Some(DaemonMessage::SwitchProtocolVersionResponse(version))
                if CLIENT_READY_FOR_LOGS.matches(&version) =>
            {
                self.agent_connection
                    .send(ClientMessage::ReadyForLogs)
                    .await;
            }
            _ => return Err(PortForwardError::AgentConnectionFailed),
        }

        loop {
            select! {
                _ = tokio::time::sleep_until(self.ping_pong_timeout.into()) => {
                    if self.waiting_for_pong {
                        // no pong received before timeout
                        break Err(PortForwardError::AgentError("agent failed to respond to Ping".into()));
                    }
                    self.agent_connection.send(ClientMessage::Ping).await;
                    self.waiting_for_pong = true;
                    self.ping_pong_timeout = Instant::now() + Duration::from_secs(30);
                },

                message = self.agent_connection.recv() => match message {
                    Some(message) => self.handle_msg_from_agent(message).await?,
                    None => {
                        break Err(PortForwardError::AgentError("unexpected end of connection with agent".into()));
                    },
                },

                // stream coming from the user app
                message = self.listeners.next() => match message {
                    Some(message) => self.handle_listener_stream(message).await?,
                    None => unreachable!("created listener sockets are never closed"),
                },

                message = self.internal_msg_rx.recv() => {
                    self.handle_msg_from_task(message.expect("this channel is never closed")).await?;
                },
            }
        }
    }

    #[tracing::instrument(level = Level::TRACE, skip(self), err, ret)]
    async fn handle_msg_from_agent(
        &mut self,
        message: DaemonMessage,
    ) -> Result<(), PortForwardError> {
        match message {
            DaemonMessage::TcpOutgoing(message) => match message {
                DaemonTcpOutgoing::Connect(res) => match res {
                    Ok(res) => {
                        let connection_id = res.connection_id;
                        let SocketAddress::Ip(remote_socket) = res.remote_address else {
                            return Err(PortForwardError::ConnectionError(
                                "unexpectedly received Unix address for socket during setup".into(),
                            ));
                        };
                        let Some((socket_pair, channel)) = self.id_oneshots.pop_front() else {
                            return Err(PortForwardError::ReadyTaskNotFound(
                                remote_socket,
                                connection_id,
                            ));
                        };
                        let port_map = ConnectionPortMapping {
                            pair: socket_pair.clone(),
                            remote: remote_socket,
                        };
                        self.sockets.insert(connection_id, port_map);
                        match channel.send(connection_id) {
                            Ok(_) => (),
                            Err(_) => {
                                self.agent_connection
                                    .send(ClientMessage::TcpOutgoing(LayerTcpOutgoing::Close(
                                        LayerClose { connection_id },
                                    )))
                                    .await;
                                self.task_txs.remove(&socket_pair);
                                self.sockets.remove(&connection_id);
                                tracing::warn!(
                                    "failed to send connection ID {connection_id} to task on oneshot channel"
                                );
                            }
                        };
                        tracing::trace!(
                            "successful connection to remote address {remote_socket}, connection ID is {}",
                            connection_id
                        );
                    }
                    Err(error) => {
                        tracing::error!("failed to connect to a remote address: {error}");
                        // LocalConnectionTask will fail when oneshot is dropped and handle cleanup
                        let _ = self.id_oneshots.pop_front();
                    }
                },
                DaemonTcpOutgoing::ConnectV2(..) => {
                    // Port forwarder does not use connect v2 variants.
                    return Err(PortForwardError::AgentError(format!(
                        "unexpected message from agent: {message:?}"
                    )));
                }
                DaemonTcpOutgoing::Read(res) => match res {
                    Ok(res) => {
                        let Some(ConnectionPortMapping {
                            pair: socket_pair,
                            remote: _,
                        }) = self.sockets.get(&res.connection_id)
                        else {
                            // ignore unknown connection IDs
                            return Ok(());
                        };
                        let Some(sender) = self.task_txs.get(socket_pair) else {
                            unreachable!("sender is always created before this point")
                        };
                        match sender.send(res.bytes.into_vec()).await {
                            Ok(_) => (),
                            Err(_) => {
                                self.task_txs.remove(socket_pair);
                                self.sockets.remove(&res.connection_id);
                                self.agent_connection
                                    .send(ClientMessage::TcpOutgoing(LayerTcpOutgoing::Close(
                                        LayerClose {
                                            connection_id: res.connection_id,
                                        },
                                    )))
                                    .await;
                                tracing::error!(
                                    "failed to send response from remote to local port"
                                );
                            }
                        }
                    }
                    Err(error) => {
                        return Err(PortForwardError::AgentError(format!(
                            "problem receiving DaemonTcpOutgoing::Read {error}"
                        )));
                    }
                },
                DaemonTcpOutgoing::Close(connection_id) => {
                    let Some(ConnectionPortMapping {
                        pair: socket_pair,
                        remote: remote_socket,
                    }) = self.sockets.remove(&connection_id)
                    else {
                        // ignore unknown connection IDs
                        return Ok(());
                    };
                    self.task_txs.remove(&socket_pair);
                    tracing::trace!(
                        "connection closed for port mapping {local_socket}:{remote_socket}, connection {connection_id}",
                        local_socket = socket_pair.local
                    );
                }
            },
            DaemonMessage::GetAddrInfoResponse(GetAddrInfoResponse(message)) => match message {
                Ok(DnsLookup(record)) if !record.is_empty() => {
                    // pop oneshot, send string
                    let resolved_ipv4: Vec<&LookupRecord> = record
                        .iter()
                        .filter(|LookupRecord { ip, .. }| ip.is_ipv4())
                        .collect();
                    // use first IPv4 is it exists, otherwise use IPv6
                    let resolved_ip = match resolved_ipv4.first() {
                        Some(first) => first.ip,
                        None => record.first().unwrap().ip,
                    };
                    let Some((socket_pair, channel)) = self.dns_oneshots.pop_front() else {
                        return Err(PortForwardError::LookupReqNotFound(resolved_ip));
                    };
                    match channel.send(resolved_ip) {
                        Ok(_) => (),
                        Err(_) => {
                            self.task_txs.remove(&socket_pair);
                            tracing::warn!(
                                "failed to send resolved ip {resolved_ip} to task on oneshot channel"
                            );
                        }
                    };
                }
                _ => {
                    // lookup failed, close task and err
                    let Some((socket_pair, _channel)) = self.dns_oneshots.pop_front() else {
                        tracing::warn!("failed to resolve remote hostname");
                        // no ready task, LocalConnectionTask will fail when oneshot is dropped and
                        // handle cleanup
                        return Ok(());
                    };
                    self.task_txs.remove(&socket_pair);
                    let remote = self.raw_mappings.get(&socket_pair.local);
                    match remote {
                        Some((remote, _)) => {
                            tracing::warn!("failed to resolve remote hostname for {remote:?}")
                        }
                        None => unreachable!("remote always exists here"),
                    }
                }
            },
            DaemonMessage::LogMessage(log_message) => match log_message.level {
                LogLevel::Warn => tracing::warn!("agent log: {}", log_message.message),
                LogLevel::Error => tracing::error!("agent log: {}", log_message.message),
                LogLevel::Info => tracing::info!("agent log: {}", log_message.message),
            },
            DaemonMessage::Close(error) => {
                return Err(PortForwardError::AgentError(error));
            }
            DaemonMessage::Pong if self.waiting_for_pong => {
                self.waiting_for_pong = false;
            }
            DaemonMessage::OperatorPing(id) => {
                self.agent_connection
                    .send(ClientMessage::OperatorPong(id))
                    .await
            }
            message @ (DaemonMessage::File(..)
            | DaemonMessage::Pong
            | DaemonMessage::Tcp(..)
            | DaemonMessage::GetEnvVarsResponse(..)
            | DaemonMessage::PauseTarget(..)
            | DaemonMessage::SwitchProtocolVersionResponse(..)
            | DaemonMessage::UdpOutgoing(..)
            | DaemonMessage::Vpn(..)
            | DaemonMessage::TcpSteal(..)
            | DaemonMessage::ReverseDnsLookup(..)) => {
                // includes unexpected DaemonMessage::Pong
                return Err(PortForwardError::AgentError(format!(
                    "unexpected message from agent: {message:?}"
                )));
            }
        }

        Ok(())
    }

    #[tracing::instrument(level = Level::TRACE, skip(self), err, ret)]
    async fn handle_listener_stream(
        &mut self,
        message: (SocketAddr, Result<TcpStream, std::io::Error>),
    ) -> Result<(), PortForwardError> {
        let local_socket = message.0;
        let stream = match message.1 {
            Ok(stream) => stream,
            Err(error) => {
                // error from TcpStream
                tracing::error!(
                    "error occurred while listening to local socket {local_socket}: {error}"
                );
                self.listeners.remove(&local_socket);
                return Ok(());
            }
        };

        let peer_socket = stream
            .peer_addr()
            .map_err(PortForwardError::TcpListenerError)?;

        let task_internal_tx = self.internal_msg_tx.clone();
        let Some(remote_socket) = self.raw_mappings.get(&local_socket).cloned() else {
            unreachable!("mappings are always created before this point")
        };
        tracing::debug!(
            ?local_socket,
            ?remote_socket,
            ?peer_socket,
            "starting new local connection task"
        );

        let socket_pair = ConnectionSocketPair {
            local: local_socket,
            peer: peer_socket,
        };

        let (response_tx, response_rx) = mpsc::channel(256);
        self.task_txs.insert(socket_pair, response_tx);

        tokio::spawn(async move {
            let mut task = LocalConnectionTask::new(
                stream,
                local_socket,
                peer_socket,
                remote_socket,
                task_internal_tx,
                response_rx,
            );
            task.run().await
        });

        Ok(())
    }

    #[tracing::instrument(level = Level::TRACE, skip(self), err, ret)]
    async fn handle_msg_from_task(
        &mut self,
        message: PortForwardMessage,
    ) -> Result<(), PortForwardError> {
        match message {
            PortForwardMessage::Lookup(socket_pair, node, oneshot) => {
                self.dns_oneshots.push_back((socket_pair, oneshot));
                self.agent_connection
                    .send(ClientMessage::GetAddrInfoRequest(GetAddrInfoRequest {
                        node,
                    }))
                    .await;
            }
            PortForwardMessage::Connect(port_mapping, oneshot) => {
                let remote_address = SocketAddress::Ip(port_mapping.remote);
                self.id_oneshots.push_back((port_mapping.pair, oneshot));
                self.agent_connection
                    .send(ClientMessage::TcpOutgoing(LayerTcpOutgoing::Connect(
                        LayerConnect { remote_address },
                    )))
                    .await;
            }
            PortForwardMessage::Send(connection_id, bytes) => {
                self.agent_connection
                    .send(ClientMessage::TcpOutgoing(LayerTcpOutgoing::Write(
                        LayerWrite {
                            connection_id,
                            bytes,
                        },
                    )))
                    .await;
            }
            PortForwardMessage::Close(socket_pair, connection_id) => {
                self.task_txs.remove(&socket_pair);
                if let Some(connection_id) = connection_id {
                    self.agent_connection
                        .send(ClientMessage::TcpOutgoing(LayerTcpOutgoing::Close(
                            LayerClose { connection_id },
                        )))
                        .await;
                    self.sockets.remove(&connection_id);
                }
            }
        }
        Ok(())
    }
}

pub struct ReversePortForwarder {
    /// communicates with the agent (only TCP supported).
    agent_connection: Connection<Client>,
    /// background task (uses [`IncomingProxy`] to communicate with layer)
    background_tasks: BackgroundTasks<(), ProxyMessage, IncomingProxyError>,
    /// incoming proxy background task tx
    incoming_proxy: TaskSender<IncomingProxy>,
    /// `true` if [`ClientMessage::Ping`] has been sent to agent and we're waiting for the the
    /// [`DaemonMessage::Pong`]
    waiting_for_pong: bool,
    ping_pong_timeout: Instant,
}

impl ReversePortForwarder {
    pub(crate) async fn new(
        mut agent_connection: Connection<Client>,
        mappings: HashMap<RemotePort, LocalPort>,
        mut network_config: IncomingConfig,
        idle_local_http_connection_timeout: Duration,
    ) -> Result<Self, PortForwardError> {
        let mut background_tasks: BackgroundTasks<(), ProxyMessage, IncomingProxyError> =
            BackgroundTasks::new(agent_connection.tx_handle());

        let incoming = background_tasks.register(
            IncomingProxy::new(
                idle_local_http_connection_timeout,
                network_config
                    .tls_delivery
                    .clone()
                    .or_else(|| network_config.https_delivery.clone())
                    .unwrap_or_default(),
            ),
            (),
            512,
        );

        agent_connection
            .send(ClientMessage::SwitchProtocolVersion(
                mirrord_protocol::VERSION.clone(),
            ))
            .await;
        let protocol_version = match agent_connection.recv().await {
            Some(DaemonMessage::SwitchProtocolVersionResponse(version)) => version,
            _ => return Err(PortForwardError::AgentConnectionFailed),
        };

        if CLIENT_READY_FOR_LOGS.matches(&protocol_version) {
            agent_connection.send(ClientMessage::ReadyForLogs).await;
        }

        let incoming_mode = IncomingMode::new(&mut network_config, &protocol_version);

        incoming
            .send(IncomingProxyMessage::AgentProtocolVersion(protocol_version))
            .await;

        for (i, (&remote, &local)) in mappings.iter().enumerate() {
            let subscription = incoming_mode.subscription(remote);
            let message_id = i as u64;
            let layer_id = LayerId(1);
            let req = IncomingRequest::PortSubscribe(PortSubscribe {
                listening_on: SocketAddr::new(Ipv4Addr::LOCALHOST.into(), local),
                subscription,
            });
            incoming
                .send(IncomingProxyMessage::LayerRequest(
                    message_id, layer_id, req,
                ))
                .await;
        }

        Ok(Self {
            agent_connection,
            background_tasks,
            incoming_proxy: incoming,
            waiting_for_pong: false,
            ping_pong_timeout: Instant::now(),
        })
    }

    pub(crate) async fn run(&mut self) -> Result<(), PortForwardError> {
        loop {
            select! {
                _ = tokio::time::sleep_until(self.ping_pong_timeout.into()) => {
                    if self.waiting_for_pong {
                        // no pong received before timeout
                        break Err(PortForwardError::AgentError("agent failed to respond to Ping".into()));
                    }
                    self.agent_connection.send(ClientMessage::Ping).await;
                    self.waiting_for_pong = true;
                    self.ping_pong_timeout = Instant::now() + Duration::from_secs(30);
                },

                message = self.agent_connection.recv() => match message {
                    Some(message) => {
                        self.handle_msg_from_agent(message).await?
                    },
                    None => {
                        break Err(PortForwardError::AgentError("unexpected end of connection with agent".into()));
                    },
                },

                Some((_, update)) = self.background_tasks.next() => {
                    self.handle_msg_from_local(update).await?
                },
            }
        }
    }

    #[tracing::instrument(level = Level::TRACE, skip(self), err)]
    async fn handle_msg_from_agent(
        &mut self,
        message: DaemonMessage,
    ) -> Result<(), PortForwardError> {
        match message {
            DaemonMessage::Tcp(msg) => {
                self.incoming_proxy
                    .send(IncomingProxyMessage::AgentMirror(msg))
                    .await
            }
            DaemonMessage::TcpSteal(msg) => {
                self.incoming_proxy
                    .send(IncomingProxyMessage::AgentSteal(msg))
                    .await
            }
            DaemonMessage::OperatorPing(id) => {
                self.agent_connection
                    .send(ClientMessage::OperatorPong(id))
                    .await
            }
            DaemonMessage::LogMessage(log_message) => match log_message.level {
                LogLevel::Warn => tracing::warn!("agent log: {}", log_message.message),
                LogLevel::Error => tracing::error!("agent log: {}", log_message.message),
                LogLevel::Info => tracing::info!("agent log: {}", log_message.message),
            },
            DaemonMessage::Close(error) => {
                return Err(PortForwardError::AgentError(error));
            }
            DaemonMessage::Pong if self.waiting_for_pong => {
                self.waiting_for_pong = false;
            }
            message @ DaemonMessage::UdpOutgoing(_)
            | message @ DaemonMessage::TcpOutgoing(_)
            | message @ DaemonMessage::File(_)
            | message @ DaemonMessage::GetEnvVarsResponse(_)
            | message @ DaemonMessage::GetAddrInfoResponse(_)
            | message @ DaemonMessage::PauseTarget(_)
            | message @ DaemonMessage::SwitchProtocolVersionResponse(_)
            | message @ DaemonMessage::Vpn(_)
            | message @ DaemonMessage::Pong
            | message @ DaemonMessage::ReverseDnsLookup(_) => {
                return Err(PortForwardError::AgentError(format!(
                    "unexpected message from agent: {message:?}"
                )));
            }
        }

        Ok(())
    }

    #[tracing::instrument(level = Level::TRACE, skip(self), err)]
    async fn handle_msg_from_local(
        &mut self,
        update: TaskUpdate<ProxyMessage, IncomingProxyError>,
    ) -> Result<(), PortForwardError> {
        match update {
            TaskUpdate::Message(message) => match message {
                ProxyMessage::ToLayer(ToLayer {
                    message: ProxyToLayerMessage::Incoming(IncomingResponse::PortSubscribe(res)),
                    ..
                }) => {
                    if let Err(error) = res {
                        return Err(IncomingProxyError::SubscriptionFailed(error).into());
                    }
                }
                other => {
                    tracing::debug!(
                        "unexpected message type from background task (message ignored): {other:?}"
                    )
                }
            },

            TaskUpdate::Finished(result) => match result {
                Ok(()) => {
                    unreachable!(
                        "IncomingProxy should not finish, task sender is alive in this struct"
                    );
                }
                Err(TaskError::Error(e)) => {
                    return Err(e.into());
                }
                Err(TaskError::Panic) => {
                    return Err(PortForwardError::IncomingProxyPanicked);
                }
            },
        }

        Ok(())
    }
}

/// Used by tasks for individual forwarding connections to send instructions to [`PortForwarder`]'s
/// main loop.
#[derive(Debug)]
enum PortForwardMessage {
    /// A request to perform lookup on the given hostname at the remote peer.
    /// Sent by the task only after receiving first batch of data from the user.
    /// The task waits for [`SocketAddr`] on the other end of the [`oneshot`] channel.
    Lookup(ConnectionSocketPair, String, oneshot::Sender<IpAddr>),

    /// A request to make outgoing connection to the remote peer.
    /// Sent by the task only after receiving first batch of data from the user and after hostname
    /// resolution (if applicable). The task waits for [`ConnectionId`] on the other end of the
    /// [`oneshot`] channel.
    Connect(ConnectionPortMapping, oneshot::Sender<ConnectionId>),

    /// Data received from the user in the connection with the given id.
    Send(ConnectionId, Payload),

    /// A request to close the remote connection with the given id, if it exists, and the local
    /// socket.
    Close(ConnectionSocketPair, Option<ConnectionId>),
}

struct LocalConnectionTask {
    /// peer address from [`TcpStream`] connected to the local port
    peer_socket: SocketAddr,
    /// read half of the TcpStream connected to the local port, wrapped in a stream
    read_stream: ReaderStream<OwnedReadHalf>,
    /// write half of the TcpStream connected to the local port
    write: Option<OwnedWriteHalf>,
    /// the mapping local_port:remote_ip:remote_port
    port_mapping: AddrPortMapping,
    /// tx for sending internal messages to the main loop
    task_internal_tx: Sender<PortForwardMessage>,
    /// rx for receiving data from the main loop
    data_rx: Receiver<Vec<u8>>,
}

impl LocalConnectionTask {
    pub fn new(
        stream: TcpStream,
        local_socket: SocketAddr,
        peer_socket: SocketAddr,
        remote_socket: (RemoteAddr, u16),
        task_internal_tx: Sender<PortForwardMessage>,
        response_rx: Receiver<Vec<u8>>,
    ) -> Self {
        let (read, write) = stream.into_split();
        let read_stream = ReaderStream::with_capacity(read, 64 * 1024);
        let port_mapping = AddrPortMapping {
            local: local_socket,
            remote: remote_socket,
        };
        Self {
            peer_socket,
            read_stream,
            write: Some(write),
            port_mapping,
            task_internal_tx,
            data_rx: response_rx,
        }
    }

    fn socket_pair(&self) -> ConnectionSocketPair {
        ConnectionSocketPair {
            local: self.port_mapping.local,
            peer: self.peer_socket,
        }
    }

    #[tracing::instrument(level = Level::TRACE, skip(self), err, ret)]
    pub async fn run(&mut self) -> Result<(), PortForwardError> {
        let (id_oneshot_tx, id_oneshot_rx) = oneshot::channel::<ConnectionId>();
        let (dns_oneshot_tx, dns_oneshot_rx) = oneshot::channel::<IpAddr>();

        let (resolved_ip, port): (IpAddr, u16) = match &self.port_mapping.remote {
            (RemoteAddr::Ip(ip), port) => (IpAddr::V4(*ip), *port),
            (RemoteAddr::Hostname(hostname), port) => {
                match self
                    .task_internal_tx
                    .send(PortForwardMessage::Lookup(
                        self.socket_pair(),
                        hostname.clone(),
                        dns_oneshot_tx,
                    ))
                    .await
                {
                    Ok(_) => (),
                    Err(error) => {
                        tracing::warn!(
                            "failed to send hostname lookup request to PortForwarder on internal channel: {error}"
                        );
                    }
                }
                // wait on oneshot for reply
                match dns_oneshot_rx.await {
                    Ok(ip) => (ip, *port),
                    Err(error) => {
                        tracing::warn!(
                            "failed to receive resolved hostname from PortForwarder on internal channel: {error}"
                        );
                        let _ = self
                            .task_internal_tx
                            .send(PortForwardMessage::Close(self.socket_pair(), None))
                            .await;
                        return Ok(());
                    }
                }
            }
        };
        let resolved_remote = SocketAddr::new(resolved_ip, port);
        let resolved_mapping = ConnectionPortMapping {
            pair: self.socket_pair(),
            remote: resolved_remote,
        };

        match self
            .task_internal_tx
            .send(PortForwardMessage::Connect(resolved_mapping, id_oneshot_tx))
            .await
        {
            Ok(_) => (),
            Err(error) => {
                tracing::warn!(
                    "failed to send connection request to PortForwarder on internal channel: {error}"
                );
            }
        };
        let connection_id = match id_oneshot_rx.await {
            Ok(connection_id) => connection_id,
            Err(error) => {
                tracing::warn!(
                    "failed to receive connection ID from PortForwarder on internal channel: {error}"
                );
                let _ = self
                    .task_internal_tx
                    .send(PortForwardMessage::Close(self.socket_pair(), None))
                    .await;
                return Ok(());
            }
        };

        let result: Result<(), PortForwardError> = loop {
            select! {
                message = self.read_stream.next() => match message {
                    Some(Ok(message)) => {
                        match self.task_internal_tx
                            .send(PortForwardMessage::Send(connection_id, message.into()))
                            .await
                        {
                            Ok(_) => (),
                            Err(error) => {
                                tracing::warn!("failed to send data to main loop: {error}");
                            }
                        };
                    },
                    Some(Err(error)) => {
                        tracing::warn!(
                            %error,
                            port_mapping = ?self.port_mapping,
                            "local connection failed",
                        );
                        break Ok(());
                    },
                    None => {
                        break Ok(());
                    },
                },

                message = self.data_rx.recv() => match message {
                    Some(message) if message.is_empty() => {
                        // ignore repeat empty messages
                        if let Some(write) = self.write.take() {
                            drop(write);
                            tracing::debug!(
                                port_mapping = ?self.port_mapping,
                                "remote half closed the connection",
                            );
                        }
                    }
                    Some(message) => {
                       // ignore messages after write half closed
                        if let Some(write) = self.write.as_mut() {
                            match write.write_all(message.as_ref()).await {
                                Ok(_) => continue,
                                Err(error) => {
                                    tracing::error!(
                                        %error,
                                        port_mapping = ?self.port_mapping,
                                        "local connection failed",
                                    );
                                    break Ok(());
                                },
                            }
                        };
                    },
                    None => break Ok(()),
                }
            }
        };

        let _ = self
            .task_internal_tx
            .send(PortForwardMessage::Close(
                self.socket_pair(),
                Some(connection_id),
            ))
            .await;
        result
    }
}

#[derive(Debug)]
pub struct IncomingMode {
    pub steal: bool,
    pub http_settings: Option<HttpSettings>,
}
#[derive(Debug)]
pub struct HttpSettings {
    /// The HTTP filter to use.
    pub filter: HttpFilter,
    /// Ports to filter HTTP on.
    pub ports: Option<HashSet<Port>>,
}

impl IncomingMode {
    /// Creates a new instance from the given [`IncomingConfig`].
    ///
    /// # Params
    ///
    /// * `config` - [`IncomingConfig`] is taken as `&mut` due to `add_probe_ports_to_http_ports`.
    fn new(config: &mut IncomingConfig, protocol_version: &Version) -> Self {
        // Only create HttpSettings if there are actual filters configured.
        if config.http_filter.is_filter_set().not() {
            return Self {
                steal: config.is_steal(),
                http_settings: None,
            };
        }

        if config.is_steal().not() && MIRROR_HTTP_FILTER_VERSION.matches(protocol_version).not() {
            tracing::warn!(
                %protocol_version,
                "Negotiated mirrord-protocol does not support using an HTTP filter when mirroring traffic. \
                The HTTP filter will be ignored."
            )
        }

        let ports = config
            .http_filter
            .ports
            .as_ref()
            .cloned()
            .map(HashSet::from);

        let filter = config
            .http_filter
            .as_protocol_http_filter()
            .expect("invalid HTTP filter expression");

        Self {
            steal: config.is_steal(),
            http_settings: Some(HttpSettings { filter, ports }),
        }
    }

    /// Returns [`PortSubscription`] request to be used for the given port.
    pub fn subscription(&self, port: Port) -> PortSubscription {
        if self.steal {
            let steal_type = match &self.http_settings {
                None => StealType::All(port),
                Some(settings) => {
                    if settings
                        .ports
                        .as_ref()
                        .is_some_and(|p| p.contains(&port).not())
                    {
                        StealType::All(port)
                    } else {
                        StealType::FilteredHttpEx(port, settings.filter.clone())
                    }
                }
            };
            PortSubscription::Steal(steal_type)
        } else {
            let mirror_type = match &self.http_settings {
                None => MirrorType::All(port),
                Some(settings) => {
                    if settings
                        .ports
                        .as_ref()
                        .is_some_and(|p| p.contains(&port).not())
                    {
                        MirrorType::All(port)
                    } else {
                        MirrorType::FilteredHttp(port, settings.filter.clone())
                    }
                }
            };
            PortSubscription::Mirror(mirror_type)
        }
    }
}

#[derive(Debug, Error)]
pub enum PortForwardError {
    #[error("multiple port forwarding mappings found for local address `{0}`")]
    PortMapSetupError(SocketAddr),

    #[error("multiple port forwarding mappings found for destination port `{0:?}`")]
    ReversePortMapSetupError(RemotePort),

    #[error("agent closed connection with error: `{0}`")]
    AgentError(String),

    #[error("connection with the agent failed")]
    AgentConnectionFailed,

    #[error("error from the IncomingProxy task: {0}")]
    IncomingProxyError(#[from] IncomingProxyError),

    #[error("IncomingProxy task panicked")]
    IncomingProxyPanicked,

    #[error("TcpListener operation failed with error: `{0}`")]
    TcpListenerError(std::io::Error),

    #[error("no task for socket {0} ready to receive connection ID: `{1}`")]
    ReadyTaskNotFound(SocketAddr, ConnectionId),

    #[error("no task ready to receive resolved ip: `{0}`")]
    LookupReqNotFound(IpAddr),

    #[error("failed to establish connection with remote process: `{0}`")]
    ConnectionError(String),
}

impl From<mpsc::error::SendError<ClientMessage>> for PortForwardError {
    fn from(_: mpsc::error::SendError<ClientMessage>) -> Self {
        Self::AgentConnectionFailed
    }
}

#[cfg(test)]
mod test {
    use std::{
        collections::HashMap,
        io::ErrorKind::WouldBlock,
        net::{IpAddr, Ipv4Addr, SocketAddr},
        time::Duration,
    };

    use mirrord_config::feature::network::incoming::{IncomingConfig, IncomingMode};
    use mirrord_protocol::{
        ClientMessage, DaemonMessage, ToPayload,
        outgoing::{
            DaemonConnect, DaemonRead, LayerConnect, LayerWrite, SocketAddress,
            tcp::{DaemonTcpOutgoing, LayerTcpOutgoing},
        },
        tcp::{
            DaemonTcp, Filter, HttpRequest, HttpResponse, InternalHttpBody, InternalHttpBodyFrame,
            InternalHttpRequest, InternalHttpResponse, LayerTcp, LayerTcpSteal, NewTcpConnectionV1,
            StealType, TcpClose, TcpData,
        },
    };
    use mirrord_protocol_io::{Client, Connection, ConnectionOutput};
    use reqwest::{Method, StatusCode, Version, header::HeaderMap};
    use rstest::rstest;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        sync::mpsc,
    };

    use crate::{
        RemoteAddr,
        port_forward::{PortForwarder, ReversePortForwarder},
    };

    /// Connects [`ReversePortForwarder`] with test code with [`ClientMessage`] and
    /// [`DaemonMessage`] channels. Runs a background [`tokio::task`] that auto responds to
    /// standard [`mirrord_protocol`] messages (e.g [`ClientMessage::Ping`]).
    struct TestAgentConnection {
        daemon_msg_tx: mpsc::Sender<DaemonMessage>,
        client_msg_rx: mpsc::Receiver<ClientMessage>,
    }

    impl TestAgentConnection {
        fn new() -> (Self, Connection<Client>) {
            let (connection, tx, rx) = Connection::dummy();

            let (filtered_tx, filtered_rx) = mpsc::channel(8);

            tokio::spawn(Self::auto_responder(rx, filtered_tx, tx.clone()));

            (
                Self {
                    daemon_msg_tx: tx,
                    client_msg_rx: filtered_rx,
                },
                connection,
            )
        }

        /// Sends the [`DaemonMessage`] to the [`ReversePortForwarder`].
        async fn send(&self, message: DaemonMessage) {
            self.daemon_msg_tx.send(message).await.unwrap();
        }

        /// Receives a [`ClientMessage`] from the [`ReversePortForwarder`].
        ///
        /// Some standard messages are handled internally and are never returned:
        /// 1. [`ClientMessage::Ping`]
        /// 2. [`ClientMessage::SwitchProtocolVersion`]
        /// 3. [`ClientMessage::ReadyForLogs`]
        async fn recv(&mut self) -> ClientMessage {
            self.client_msg_rx.recv().await.unwrap()
        }

        async fn auto_responder(
            rx: ConnectionOutput<Client>,
            tx_to_test_code: mpsc::Sender<ClientMessage>,
            tx_to_port_forwarder: mpsc::Sender<DaemonMessage>,
        ) {
            loop {
                let Some(message) = rx.next().await else {
                    break;
                };

                match message {
                    ClientMessage::Ping => {
                        tx_to_port_forwarder
                            .send(DaemonMessage::Pong)
                            .await
                            .unwrap();
                    }
                    ClientMessage::ReadyForLogs => {}
                    ClientMessage::SwitchProtocolVersion(version) => {
                        tx_to_port_forwarder
                            .send(DaemonMessage::SwitchProtocolVersionResponse(
                                std::cmp::min(&version, &*mirrord_protocol::VERSION).clone(),
                            ))
                            .await
                            .unwrap();
                    }
                    other => tx_to_test_code.send(other).await.unwrap(),
                }
            }
        }
    }

    #[tokio::test]
    async fn single_port_forwarding() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local_destination = listener.local_addr().unwrap();
        drop(listener);

        let (mut test_connection, agent_connection) = TestAgentConnection::new();

        let remote_ip = "152.37.40.40".parse::<Ipv4Addr>().unwrap();
        let remote_destination = (RemoteAddr::Ip(remote_ip), 3038);
        let mappings = HashMap::from([(local_destination, remote_destination.clone())]);

        // Prepare listeners before sending work to the background task.
        let mut port_forwarder = PortForwarder::new(agent_connection, mappings)
            .await
            .unwrap();
        tokio::spawn(async move { port_forwarder.run().await.unwrap() });

        // Connect to PortForwarders listener and send some data to trigger remote connection
        // request.
        let mut stream = TcpStream::connect(local_destination).await.unwrap();
        stream.write_all(b"data-my-beloved").await.unwrap();

        // Expect a connection request
        let remote_address = SocketAddress::Ip(SocketAddr::new(remote_ip.into(), 3038));
        let expected = ClientMessage::TcpOutgoing(LayerTcpOutgoing::Connect(LayerConnect {
            remote_address: remote_address.clone(),
        }));
        assert_eq!(test_connection.recv().await, expected,);

        // Reply with successful on daemon_msg_tx
        test_connection
            .send(DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Connect(Ok(
                DaemonConnect {
                    connection_id: 1,
                    remote_address,
                    local_address: "1.2.3.4:2137".parse::<SocketAddr>().unwrap().into(),
                },
            ))))
            .await;

        let expected = ClientMessage::TcpOutgoing(LayerTcpOutgoing::Write(LayerWrite {
            connection_id: 1,
            bytes: b"data-my-beloved".to_payload(),
        }));
        assert_eq!(test_connection.recv().await, expected);

        // Send response data from agent on daemon_msg_tx
        test_connection
            .send(DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Read(Ok(
                DaemonRead {
                    connection_id: 1,
                    bytes: b"reply-my-beloved".to_payload(),
                },
            ))))
            .await;

        // Check data arrives at local
        let mut buf = [0; 16];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(buf, b"reply-my-beloved".as_ref());

        // Half closure: check that the (local port) read half of the stream is still open
        assert_eq!(
            stream
                .try_read(&mut buf)
                .expect_err("reading should give `WouldBlock` error (no data to read)")
                .kind(),
            WouldBlock
        );

        // Half close the connection (emulate sending FIN from remote)
        test_connection
            .send(DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Read(Ok(
                DaemonRead {
                    connection_id: 1,
                    bytes: "".to_payload(),
                },
            ))))
            .await;

        // Wait for the socket to be readable
        stream.readable().await.unwrap();

        // Check that the (local port) read half of the stream has closed
        assert_eq!(stream.try_read(&mut buf).unwrap(), 0);
    }

    #[tokio::test]
    async fn multiple_mappings_port_forwarding() {
        let remote_destination_1 = (RemoteAddr::Ip("152.37.40.40".parse().unwrap()), 1018);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local_destination_1 = listener.local_addr().unwrap();
        drop(listener);

        let remote_destination_2 = (RemoteAddr::Ip("152.37.40.40".parse().unwrap()), 2028);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local_destination_2 = listener.local_addr().unwrap();
        drop(listener);

        let (mut test_connection, agent_connection) = TestAgentConnection::new();
        let mappings = HashMap::from([
            (local_destination_1, remote_destination_1.clone()),
            (local_destination_2, remote_destination_2.clone()),
        ]);

        // Prepare listeners before sending work to the background task.
        let mut port_forwarder = PortForwarder::new(agent_connection, mappings)
            .await
            .unwrap();
        tokio::spawn(async move { port_forwarder.run().await.unwrap() });

        // send data to first socket
        let mut stream_1 = TcpStream::connect(local_destination_1).await.unwrap();

        // expect each Connect on client_msg_rx with correct mappings when data has been written
        // (lazy)
        stream_1.write_all(b"data-from-1").await.unwrap();
        let RemoteAddr::Ip(ip) = remote_destination_1.0 else {
            unreachable!()
        };
        let remote_address_1 =
            SocketAddress::Ip(SocketAddr::new(IpAddr::V4(ip), remote_destination_1.1));
        let expected = ClientMessage::TcpOutgoing(LayerTcpOutgoing::Connect(LayerConnect {
            remote_address: remote_address_1.clone(),
        }));
        assert_eq!(test_connection.recv().await, expected);

        // send data to second socket
        let mut stream_2 = TcpStream::connect(local_destination_2).await.unwrap();
        let RemoteAddr::Ip(ip) = remote_destination_2.0 else {
            unreachable!()
        };
        let remote_address_2 =
            SocketAddress::Ip(SocketAddr::new(IpAddr::V4(ip), remote_destination_2.1));
        stream_2.write_all(b"data-from-2").await.unwrap();

        let expected = ClientMessage::TcpOutgoing(LayerTcpOutgoing::Connect(LayerConnect {
            remote_address: remote_address_2.clone(),
        }));
        assert_eq!(test_connection.recv().await, expected);

        // reply with successful on each daemon_msg_tx
        test_connection
            .send(DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Connect(Ok(
                DaemonConnect {
                    connection_id: 1,
                    remote_address: remote_address_1.clone(),
                    local_address: remote_address_1,
                },
            ))))
            .await;
        test_connection
            .send(DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Connect(Ok(
                DaemonConnect {
                    connection_id: 2,
                    remote_address: remote_address_2.clone(),
                    local_address: remote_address_2,
                },
            ))))
            .await;

        // expect data to be received
        let expected = ClientMessage::TcpOutgoing(LayerTcpOutgoing::Write(LayerWrite {
            connection_id: 1,
            bytes: b"data-from-1".to_payload(),
        }));
        assert_eq!(test_connection.recv().await, expected);

        let expected = ClientMessage::TcpOutgoing(LayerTcpOutgoing::Write(LayerWrite {
            connection_id: 2,
            bytes: b"data-from-2".to_payload(),
        }));
        assert_eq!(test_connection.recv().await, expected);

        // send each data response from agent on daemon_msg_tx
        test_connection
            .send(DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Read(Ok(
                DaemonRead {
                    connection_id: 1,
                    bytes: b"reply-to-1".to_payload(),
                },
            ))))
            .await;
        test_connection
            .send(DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Read(Ok(
                DaemonRead {
                    connection_id: 2,
                    bytes: b"reply-to-2".to_payload(),
                },
            ))))
            .await;

        // check data arrives at each local addr
        let mut buf = [0; 10];
        stream_1.read_exact(&mut buf).await.unwrap();
        assert_eq!(buf, b"reply-to-1".as_ref());
        let mut buf = [0; 10];
        stream_2.read_exact(&mut buf).await.unwrap();
        assert_eq!(buf, b"reply-to-2".as_ref());
    }

    #[rstest]
    #[tokio::test]
    #[timeout(Duration::from_secs(5))]
    async fn reverse_port_forwarding_mirror() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local_destination = listener.local_addr().unwrap();

        let remote_address = IpAddr::from("152.37.40.40".parse::<Ipv4Addr>().unwrap());
        let destination_port = 3038;
        let mappings = HashMap::from([(destination_port, local_destination.port())]);
        let network_config = IncomingConfig::default();

        let (mut test_connection, agent_connection) = TestAgentConnection::new();

        tokio::spawn(async move {
            ReversePortForwarder::new(
                agent_connection,
                mappings,
                network_config,
                Duration::from_secs(3),
            )
            .await
            .unwrap()
            .run()
            .await
            .unwrap()
        });

        // expect port subscription for remote port and send subscribe result
        let expected = ClientMessage::Tcp(LayerTcp::PortSubscribe(destination_port));
        assert_eq!(test_connection.recv().await, expected);
        test_connection
            .send(DaemonMessage::Tcp(DaemonTcp::SubscribeResult(Ok(
                destination_port,
            ))))
            .await;

        // send new connection from agent and some data
        test_connection
            .send(DaemonMessage::Tcp(DaemonTcp::NewConnectionV1(
                NewTcpConnectionV1 {
                    connection_id: 1,
                    remote_address,
                    destination_port,
                    source_port: local_destination.port(),
                    local_address: local_destination.ip(),
                },
            )))
            .await;
        let mut stream = listener.accept().await.unwrap().0;

        test_connection
            .send(DaemonMessage::Tcp(DaemonTcp::Data(TcpData {
                connection_id: 1,
                bytes: b"data-my-beloved".to_payload(),
            })))
            .await;

        // check data arrives at local
        let mut buf = [0; 15];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(buf, b"data-my-beloved".as_ref());

        // ensure graceful behaviour on close
        test_connection
            .send(DaemonMessage::Tcp(DaemonTcp::Close(TcpClose {
                connection_id: 1,
            })))
            .await;
    }

    #[rstest]
    #[tokio::test]
    #[timeout(Duration::from_secs(5))]
    async fn reverse_port_forwarding_steal() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local_destination = listener.local_addr().unwrap();

        let remote_address = IpAddr::from("152.37.40.40".parse::<Ipv4Addr>().unwrap());
        let destination_port = 3038;
        let mappings = HashMap::from([(destination_port, local_destination.port())]);
        let network_config = IncomingConfig {
            mode: IncomingMode::Steal,
            ..Default::default()
        };

        let (mut test_connection, agent_connection) = TestAgentConnection::new();
        tokio::spawn(async move {
            ReversePortForwarder::new(
                agent_connection,
                mappings,
                network_config,
                Duration::from_secs(3),
            )
            .await
            .unwrap()
            .run()
            .await
            .unwrap()
        });

        // expect port subscription for remote port and send subscribe result
        let expected = ClientMessage::TcpSteal(LayerTcpSteal::PortSubscribe(StealType::All(
            destination_port,
        )));
        assert_eq!(test_connection.recv().await, expected);
        test_connection
            .send(DaemonMessage::TcpSteal(DaemonTcp::SubscribeResult(Ok(
                destination_port,
            ))))
            .await;

        // send new connection from agent and some data
        test_connection
            .send(DaemonMessage::TcpSteal(DaemonTcp::NewConnectionV1(
                NewTcpConnectionV1 {
                    connection_id: 1,
                    remote_address,
                    destination_port,
                    source_port: 2137,
                    local_address: "1.2.3.4".parse().unwrap(),
                },
            )))
            .await;
        let mut stream = listener.accept().await.unwrap().0;

        test_connection
            .send(DaemonMessage::TcpSteal(DaemonTcp::Data(TcpData {
                connection_id: 1,
                bytes: b"data-my-beloved".to_payload(),
            })))
            .await;

        // check data arrives at local
        let mut buf = [0; 15];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(buf, b"data-my-beloved".as_ref());

        // check for response from local
        stream.write_all(b"reply-my-beloved").await.unwrap();
        assert_eq!(
            test_connection.recv().await,
            ClientMessage::TcpSteal(LayerTcpSteal::Data(TcpData {
                connection_id: 1,
                bytes: b"reply-my-beloved".to_payload()
            }))
        );

        // ensure graceful behaviour on close
        test_connection
            .send(DaemonMessage::Tcp(DaemonTcp::Close(TcpClose {
                connection_id: 1,
            })))
            .await;
    }

    #[rstest]
    #[tokio::test]
    #[timeout(Duration::from_secs(5))]
    async fn reverse_multiple_mappings_forwarding_mirror() {
        // uses mirror mode so no responses expected
        let listener_1 = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let listener_2 = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local_destination_1 = listener_1.local_addr().unwrap();
        let local_destination_2 = listener_2.local_addr().unwrap();

        let remote_address = IpAddr::from("152.37.40.40".parse::<Ipv4Addr>().unwrap());
        let destination_port_1 = 3038;
        let destination_port_2 = 4048;
        let mappings = HashMap::from([
            (destination_port_1, local_destination_1.port()),
            (destination_port_2, local_destination_2.port()),
        ]);
        let network_config = IncomingConfig::default();

        let (mut test_connection, agent_connection) = TestAgentConnection::new();
        tokio::spawn(async move {
            let mut port_forwarder = ReversePortForwarder::new(
                agent_connection,
                mappings,
                network_config,
                Duration::from_secs(3),
            )
            .await
            .unwrap();
            port_forwarder.run().await.unwrap()
        });

        // expect port subscription for each remote port and send subscribe result
        // matches! used because order may be random
        for _ in 0..2 {
            let message = test_connection.recv().await;
            assert!(
                matches!(message, ClientMessage::Tcp(LayerTcp::PortSubscribe(_))),
                "expected ClientMessage::Tcp(LayerTcp::PortSubscribe(_), received {message:?}"
            );
        }

        test_connection
            .send(DaemonMessage::Tcp(DaemonTcp::SubscribeResult(Ok(
                destination_port_1,
            ))))
            .await;
        test_connection
            .send(DaemonMessage::Tcp(DaemonTcp::SubscribeResult(Ok(
                destination_port_2,
            ))))
            .await;

        // send new connections from agent and some data
        test_connection
            .send(DaemonMessage::Tcp(DaemonTcp::NewConnectionV1(
                NewTcpConnectionV1 {
                    connection_id: 1,
                    remote_address,
                    destination_port: destination_port_1,
                    source_port: local_destination_1.port(),
                    local_address: local_destination_1.ip(),
                },
            )))
            .await;
        let mut stream_1 = listener_1.accept().await.unwrap().0;

        test_connection
            .send(DaemonMessage::Tcp(DaemonTcp::NewConnectionV1(
                NewTcpConnectionV1 {
                    connection_id: 2,
                    remote_address,
                    destination_port: destination_port_2,
                    source_port: local_destination_2.port(),
                    local_address: local_destination_2.ip(),
                },
            )))
            .await;
        let mut stream_2 = listener_2.accept().await.unwrap().0;

        test_connection
            .send(DaemonMessage::Tcp(DaemonTcp::Data(TcpData {
                connection_id: 1,
                bytes: b"connection-1-my-beloved".to_payload(),
            })))
            .await;

        test_connection
            .send(DaemonMessage::Tcp(DaemonTcp::Data(TcpData {
                connection_id: 2,
                bytes: b"connection-2-my-beloved".to_payload(),
            })))
            .await;

        // check data arrives at local
        let mut buf = [0; 23];
        stream_1.read_exact(&mut buf).await.unwrap();
        assert_eq!(buf, b"connection-1-my-beloved".as_ref());

        let mut buf = [0; 23];
        stream_2.read_exact(&mut buf).await.unwrap();
        assert_eq!(buf, b"connection-2-my-beloved".as_ref());

        // ensure graceful behaviour on close
        test_connection
            .send(DaemonMessage::Tcp(DaemonTcp::Close(TcpClose {
                connection_id: 1,
            })))
            .await;

        test_connection
            .send(DaemonMessage::Tcp(DaemonTcp::Close(TcpClose {
                connection_id: 2,
            })))
            .await;
    }

    #[rstest]
    #[tokio::test]
    #[timeout(Duration::from_secs(5))]
    async fn filtered_reverse_port_forwarding() {
        // simulates filtered stealing with one port mapping
        // filters are matched in the agent but this tests Http type messages
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local_destination = listener.local_addr().unwrap();

        let destination_port = 8080;
        let mappings = HashMap::from([(destination_port, local_destination.port())]);
        let mut network_config = IncomingConfig {
            mode: IncomingMode::Steal,
            ..Default::default()
        };
        network_config.http_filter.header_filter = Some("header: value".to_string());

        let (mut test_connection, agent_connection) = TestAgentConnection::new();

        tokio::spawn(async move {
            let mut port_forwarder = ReversePortForwarder::new(
                agent_connection,
                mappings,
                network_config,
                Duration::from_secs(3),
            )
            .await
            .unwrap();
            port_forwarder.run().await.unwrap()
        });

        assert_eq!(
            test_connection.recv().await,
            ClientMessage::TcpSteal(LayerTcpSteal::PortSubscribe(StealType::FilteredHttpEx(
                destination_port,
                mirrord_protocol::tcp::HttpFilter::Header(
                    Filter::new("header: value".to_string()).unwrap()
                )
            ),))
        );
        test_connection
            .send(DaemonMessage::TcpSteal(DaemonTcp::SubscribeResult(Ok(
                destination_port,
            ))))
            .await;

        // send data from agent with correct header
        let mut headers = HeaderMap::new();
        headers.insert("header", "value".parse().unwrap());
        let internal_request = InternalHttpRequest {
            method: Method::GET,
            uri: "https://www.rust-lang.org/install.html".parse().unwrap(),
            headers,
            version: Version::HTTP_11,
            body: vec![].into(),
        };
        test_connection
            .send(DaemonMessage::TcpSteal(DaemonTcp::HttpRequest(
                HttpRequest {
                    internal_request,
                    connection_id: 0,
                    request_id: 0,
                    port: destination_port,
                },
            )))
            .await;

        let mut stream = listener.accept().await.unwrap().0;
        // check data is read from stream
        let mut buf = [0; 15];
        assert_eq!(buf, [0; 15]);
        stream.read_exact(&mut buf).await.unwrap();

        // check for response from local
        stream
            .write_all("HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nyay".as_bytes())
            .await
            .unwrap();

        let mut headers = HeaderMap::new();
        headers.insert("content-length", "3".parse().unwrap());
        let internal_response = InternalHttpResponse {
            status: StatusCode::OK,
            version: Version::HTTP_11,
            headers,
            body: InternalHttpBody(
                [InternalHttpBodyFrame::Data(b"yay".to_payload())]
                    .into_iter()
                    .collect(),
            ),
        };
        let expected_response =
            ClientMessage::TcpSteal(LayerTcpSteal::HttpResponseFramed(HttpResponse {
                connection_id: 0,
                request_id: 0,
                port: destination_port,
                internal_response,
            }));

        assert_eq!(test_connection.recv().await, expected_response);

        // ensure graceful behaviour on close
        test_connection
            .send(DaemonMessage::TcpSteal(DaemonTcp::Close(TcpClose {
                connection_id: 0,
            })))
            .await;
    }
}
