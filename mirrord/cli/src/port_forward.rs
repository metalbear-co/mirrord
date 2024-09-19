use std::{
    collections::{HashMap, HashSet, VecDeque},
    net::{IpAddr, SocketAddr},
    time::{Duration, Instant},
};

use futures::StreamExt;
use mirrord_protocol::{
    dns::{DnsLookup, GetAddrInfoRequest, GetAddrInfoResponse, LookupRecord},
    outgoing::{
        tcp::{DaemonTcpOutgoing, LayerTcpOutgoing},
        LayerClose, LayerConnect, LayerWrite, SocketAddress,
    },
    tcp::{
        ChunkedRequest, DaemonTcp, HttpRequest, LayerTcp, LayerTcpSteal, NewTcpConnection,
        StealType, TcpClose, TcpData,
    },
    ClientMessage, ConnectionId, DaemonMessage, LogLevel, ResponseError, CLIENT_READY_FOR_LOGS,
};
use thiserror::Error;
use tokio::{
    io::AsyncWriteExt,
    net::{
        tcp::{OwnedReadHalf, OwnedWriteHalf},
        TcpListener, TcpStream,
    },
    select,
    sync::{
        mpsc::{self, Receiver, Sender},
        oneshot,
    },
};
use tokio_stream::{wrappers::TcpListenerStream, StreamMap};
use tokio_util::io::ReaderStream;
use tracing::Level;

use crate::{
    connection::AgentConnection, AddrPortMapping, LocalPort, PortOnlyMapping, RemoteAddr,
    RemotePort,
};

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedPortMapping {
    pub local: SocketAddr,
    pub remote: SocketAddr,
}

pub struct PortForwarder {
    /// communicates with the agent (only TCP supported)
    agent_connection: AgentConnection,
    /// associates local ports with destination ports
    /// destinations may contain unresolved hostnames
    raw_mappings: HashMap<SocketAddr, (RemoteAddr, u16)>,
    /// accepts connections from the user app in the form of a stream
    listeners: StreamMap<SocketAddr, TcpListenerStream>,
    /// oneshot channels for sending connection IDs to tasks and the associated local address
    id_oneshots: VecDeque<(SocketAddr, oneshot::Sender<ConnectionId>)>,
    /// oneshot channels for sending resolved hostnames to tasks and the associated local address
    dns_oneshots: VecDeque<(SocketAddr, oneshot::Sender<IpAddr>)>,
    /// identifies a pair of mapped socket addresses by their corresponding connection ID
    sockets: HashMap<ConnectionId, ResolvedPortMapping>,
    /// identifies task senders by their corresponding local socket address
    /// for sending data from the remote socket to the local address
    task_txs: HashMap<SocketAddr, Sender<Vec<u8>>>,

    /// transmit internal messages from tasks to [`PortForwarder`]'s main loop.
    internal_msg_tx: Sender<PortForwardMessage>,
    internal_msg_rx: Receiver<PortForwardMessage>,

    /// true if Ping has been sent to agent
    waiting_for_pong: bool,
    ping_pong_timeout: Instant,
}

impl PortForwarder {
    pub(crate) async fn new(
        agent_connection: AgentConnection,
        parsed_mappings: Vec<AddrPortMapping>,
    ) -> Result<Self, PortForwardError> {
        // open tcp listener for local addrs
        let mut listeners = StreamMap::with_capacity(parsed_mappings.len());
        let mut mappings: HashMap<SocketAddr, (RemoteAddr, u16)> =
            HashMap::with_capacity(parsed_mappings.len());

        if parsed_mappings.is_empty() {
            return Err(PortForwardError::NoMappingsError());
        }

        for mapping in parsed_mappings {
            if listeners.contains_key(&mapping.local) {
                // two mappings shared a key thus keys were not unique
                return Err(PortForwardError::PortMapSetupError(mapping.local));
            }

            let listener = TcpListener::bind(mapping.local).await;
            match listener {
                Ok(listener) => {
                    listeners.insert(mapping.local, TcpListenerStream::new(listener));
                    mappings.insert(mapping.local, mapping.remote);
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
            .sender
            .send(ClientMessage::SwitchProtocolVersion(
                mirrord_protocol::VERSION.clone(),
            ))
            .await?;
        match self.agent_connection.receiver.recv().await {
            Some(DaemonMessage::SwitchProtocolVersionResponse(version))
                if CLIENT_READY_FOR_LOGS.matches(&version) =>
            {
                self.agent_connection
                    .sender
                    .send(ClientMessage::ReadyForLogs)
                    .await?;
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
                    self.agent_connection.sender.send(ClientMessage::Ping).await?;
                    self.waiting_for_pong = true;
                    self.ping_pong_timeout = Instant::now() + Duration::from_secs(30);
                },

                message = self.agent_connection.receiver.recv() => match message {
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

    #[tracing::instrument(level = Level::TRACE, skip(self), err)]
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
                        let Some((local_socket, channel)) = self.id_oneshots.pop_front() else {
                            return Err(PortForwardError::ReadyTaskNotFound(
                                remote_socket,
                                connection_id,
                            ));
                        };
                        let port_map = ResolvedPortMapping {
                            local: local_socket,
                            remote: remote_socket,
                        };
                        self.sockets.insert(connection_id, port_map);
                        match channel.send(connection_id) {
                            Ok(_) => (),
                            Err(_) => {
                                self.agent_connection
                                    .sender
                                    .send(ClientMessage::TcpOutgoing(LayerTcpOutgoing::Close(
                                        LayerClose { connection_id },
                                    )))
                                    .await?;
                                self.task_txs.remove(&local_socket);
                                self.sockets.remove(&connection_id);
                                tracing::warn!("failed to send connection ID {connection_id} to task on oneshot channel");
                            }
                        };
                        tracing::trace!("successful connection to remote address {remote_socket}, connection ID is {}", connection_id);
                    }
                    Err(error) => {
                        tracing::error!("failed to connect to a remote address: {error}");
                        // LocalConnectionTask will fail when oneshot is dropped and handle cleanup
                        let _ = self.id_oneshots.pop_front();
                    }
                },
                DaemonTcpOutgoing::Read(res) => match res {
                    Ok(res) => {
                        let Some(&ResolvedPortMapping {
                            local: local_socket,
                            remote: _,
                        }) = self.sockets.get(&res.connection_id)
                        else {
                            // ignore unknown connection IDs
                            return Ok(());
                        };
                        let Some(sender) = self.task_txs.get(&local_socket) else {
                            unreachable!("sender is always created before this point")
                        };
                        match sender.send(res.bytes).await {
                            Ok(_) => (),
                            Err(_) => {
                                self.task_txs.remove(&local_socket);
                                self.sockets.remove(&res.connection_id);
                                self.agent_connection
                                    .sender
                                    .send(ClientMessage::TcpOutgoing(LayerTcpOutgoing::Close(
                                        LayerClose {
                                            connection_id: res.connection_id,
                                        },
                                    )))
                                    .await?;
                                tracing::error!(
                                    "failed to send response from remote to local port"
                                );
                            }
                        }
                    }
                    Err(error) => {
                        return Err(PortForwardError::AgentError(format!(
                            "problem receiving DaemonTcpOutgoing::Read {error}"
                        )))
                    }
                },
                DaemonTcpOutgoing::Close(connection_id) => {
                    let Some(ResolvedPortMapping {
                        local: local_socket,
                        remote: remote_socket,
                    }) = self.sockets.remove(&connection_id)
                    else {
                        // ignore unknown connection IDs
                        return Ok(());
                    };
                    self.task_txs.remove(&local_socket);
                    tracing::trace!(
                        "connection closed for port mapping {local_socket}:{remote_socket}, connection {connection_id}"
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
                    let Some((local_socket, channel)) = self.dns_oneshots.pop_front() else {
                        return Err(PortForwardError::LookupReqNotFound(resolved_ip));
                    };
                    match channel.send(resolved_ip) {
                        Ok(_) => (),
                        Err(_) => {
                            self.task_txs.remove(&local_socket);
                            tracing::warn!("failed to send resolved ip {resolved_ip} to task on oneshot channel");
                        }
                    };
                }
                _ => {
                    // lookup failed, close task and err
                    let Some((local_socket, _channel)) = self.dns_oneshots.pop_front() else {
                        tracing::warn!("failed to resolve remote hostname");
                        // no ready task, LocalConnectionTask will fail when oneshot is dropped and
                        // handle cleanup
                        return Ok(());
                    };
                    self.task_txs.remove(&local_socket);
                    let remote = self.raw_mappings.get(&local_socket);
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
            },
            DaemonMessage::Close(error) => {
                return Err(PortForwardError::AgentError(error));
            }
            DaemonMessage::Pong if self.waiting_for_pong => {
                self.waiting_for_pong = false;
            }
            other => {
                // includes unexepcted DaemonMessage::Pong
                return Err(PortForwardError::AgentError(format!(
                    "unexpected message from agent: {other:?}"
                )));
            }
        }

        Ok(())
    }

    #[tracing::instrument(level = Level::TRACE, skip(self), err)]
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
                    "error occured while listening to local socket {local_socket}: {error}"
                );
                self.listeners.remove(&local_socket);
                return Ok(());
            }
        };

        let task_internal_tx = self.internal_msg_tx.clone();
        let Some(remote_socket) = self.raw_mappings.get(&local_socket).cloned() else {
            unreachable!("mappings are always created before this point")
        };
        let (response_tx, response_rx) = mpsc::channel(256);
        self.task_txs.insert(local_socket, response_tx);

        tokio::spawn(async move {
            let mut task = LocalConnectionTask::new(
                stream,
                local_socket,
                remote_socket,
                task_internal_tx,
                response_rx,
            );
            task.run().await
        });

        Ok(())
    }

    #[tracing::instrument(level = Level::TRACE, skip(self), err)]
    async fn handle_msg_from_task(
        &mut self,
        message: PortForwardMessage,
    ) -> Result<(), PortForwardError> {
        match message {
            PortForwardMessage::Lookup(local, node, oneshot) => {
                self.dns_oneshots.push_back((local, oneshot));
                self.agent_connection
                    .sender
                    .send(ClientMessage::GetAddrInfoRequest(GetAddrInfoRequest {
                        node,
                    }))
                    .await?;
            }
            PortForwardMessage::Connect(port_mapping, oneshot) => {
                let remote_address = SocketAddress::Ip(port_mapping.remote);
                self.id_oneshots.push_back((port_mapping.local, oneshot));
                self.agent_connection
                    .sender
                    .send(ClientMessage::TcpOutgoing(LayerTcpOutgoing::Connect(
                        LayerConnect { remote_address },
                    )))
                    .await?;
            }
            PortForwardMessage::Send(connection_id, bytes) => {
                self.agent_connection
                    .sender
                    .send(ClientMessage::TcpOutgoing(LayerTcpOutgoing::Write(
                        LayerWrite {
                            connection_id,
                            bytes,
                        },
                    )))
                    .await?;
            }
            PortForwardMessage::Close(local_socket, connection_id) => {
                self.task_txs.remove(&local_socket);
                if let Some(connection_id) = connection_id {
                    self.agent_connection
                        .sender
                        .send(ClientMessage::TcpOutgoing(LayerTcpOutgoing::Close(
                            LayerClose { connection_id },
                        )))
                        .await?;
                    self.sockets.remove(&connection_id);
                }
            }
        }
        Ok(())
    }
}

pub struct ReversePortForwarder {
    /// if true, traffic is stolen from remote.
    /// otherwise, traffic is mirrored
    steal_mode: bool,
    /// communicates with the agent (only TCP supported).
    agent_connection: AgentConnection,
    /// associates resolved destination ports with local ports.
    mappings: HashMap<RemotePort, LocalPort>,
    /// identifies a pair of mapped socket addresses by their corresponding connection ID.
    mappings_by_connection: HashMap<ConnectionId, PortOnlyMapping>,
    /// identifies task senders by their corresponding connection ID.
    task_txs: HashMap<ConnectionId, Sender<Vec<u8>>>,

    /// transmit internal messages from tasks to [`PortForwarder`]'s main loop.
    internal_msg_tx: Sender<PortForwardMessage>,
    internal_msg_rx: Receiver<PortForwardMessage>,

    /// true if Ping has been sent to agent.
    waiting_for_pong: bool,
    ping_pong_timeout: Instant,
}

impl ReversePortForwarder {
    pub(crate) async fn new(
        agent_connection: AgentConnection,
        parsed_mappings: Vec<PortOnlyMapping>,
        steal_mode: bool,
    ) -> Result<Self, PortForwardError> {
        let mut mappings: HashMap<RemotePort, LocalPort> =
            HashMap::with_capacity(parsed_mappings.len());

        if parsed_mappings.is_empty() {
            return Err(PortForwardError::NoMappingsError());
        }

        for mapping in &parsed_mappings {
            // check destinations are unique
            if mappings.contains_key(&mapping.remote) {
                // two mappings shared a key thus keys were not unique
                return Err(PortForwardError::ReversePortMapSetupError(mapping.remote));
            }
            mappings.insert(mapping.remote, mapping.local);
        }

        let (internal_msg_tx, internal_msg_rx) = mpsc::channel(1024);

        Ok(Self {
            steal_mode,
            agent_connection,
            mappings,
            mappings_by_connection: HashMap::new(),
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
            .sender
            .send(ClientMessage::SwitchProtocolVersion(
                mirrord_protocol::VERSION.clone(),
            ))
            .await?;
        match self.agent_connection.receiver.recv().await {
            Some(DaemonMessage::SwitchProtocolVersionResponse(version))
                if CLIENT_READY_FOR_LOGS.matches(&version) =>
            {
                self.agent_connection
                    .sender
                    .send(ClientMessage::ReadyForLogs)
                    .await?;
            }
            _ => return Err(PortForwardError::AgentConnectionFailed),
        }

        for remote_port in self.mappings.keys() {
            match self.steal_mode {
                true => {
                    self.agent_connection
                        .sender
                        .send(ClientMessage::TcpSteal(LayerTcpSteal::PortSubscribe(
                            StealType::All(*remote_port),
                        )))
                        .await?
                }
                false => {
                    self.agent_connection
                        .sender
                        .send(ClientMessage::Tcp(LayerTcp::PortSubscribe(*remote_port)))
                        .await?
                }
            }
        }

        loop {
            select! {
                _ = tokio::time::sleep_until(self.ping_pong_timeout.into()) => {
                    if self.waiting_for_pong {
                        // no pong received before timeout
                        break Err(PortForwardError::AgentError("agent failed to respond to Ping".into()));
                    }
                    self.agent_connection.sender.send(ClientMessage::Ping).await?;
                    self.waiting_for_pong = true;
                    self.ping_pong_timeout = Instant::now() + Duration::from_secs(30);
                },

                message = self.agent_connection.receiver.recv() => match message {
                    Some(message) => self.handle_msg_from_agent(message).await?,
                    None => {
                        break Err(PortForwardError::AgentError("unexpected end of connection with agent".into()));
                    },
                },

                message = self.internal_msg_rx.recv() => {
                    self.handle_msg_from_task(message.expect("this channel is never closed")).await?;
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
            DaemonMessage::Tcp(_tcp_msg) => {
                // TODO: mirror mode
                return Err(PortForwardError::AgentError(format!(
                    "unexpected DaemonMessage::Tcp message from agent (reverse port forwarding in mirror mode not yet implemented)"
                )));
            }
            DaemonMessage::TcpSteal(message) => match message {
                DaemonTcp::NewConnection(NewTcpConnection {
                    connection_id,
                    destination_port,
                    ..
                }) => {
                    let Some(&local_port) = self.mappings.get(&destination_port) else {
                        // ignore mappings that aren't recognised
                        tracing::debug!("connection received from agent for unrecognised remote port {destination_port}");
                        return Ok(());
                    };
                    self.mappings_by_connection.insert(
                        connection_id,
                        PortOnlyMapping {
                            local: local_port,
                            remote: destination_port,
                        },
                    );

                    let task_internal_tx = self.internal_msg_tx.clone();
                    let (response_tx, response_rx) = mpsc::channel(256);
                    self.task_txs.insert(connection_id, response_tx);

                    let stream =
                        match TcpStream::connect(format!("127.0.0.1:{destination_port}")).await {
                            Ok(stream) => stream,
                            Err(error) => return Err(PortForwardError::TcpStreamError(error)),
                        };

                    tokio::spawn(async move {
                        let mut task = LocalConnectionTask::new_reverse(
                            stream,
                            local_port,
                            destination_port,
                            task_internal_tx,
                            response_rx,
                        );
                        task.run_reverse().await
                    });
                }
                // data
                DaemonTcp::Data(TcpData {
                    connection_id,
                    bytes,
                }) => {
                    let Some(tx) = self.task_txs.get(&connection_id) else {
                        // ignore unknown connection IDs
                        return Ok(());
                    };
                    tx.send(bytes).await?;
                }
                DaemonTcp::HttpRequest(HttpRequest {
                    internal_request,
                    connection_id,
                    request_id,
                    port,
                }) => todo!(),
                DaemonTcp::HttpRequestFramed(HttpRequest {
                    internal_request,
                    connection_id,
                    request_id,
                    port,
                }) => todo!(),
                DaemonTcp::HttpRequestChunked(chunked_request) => todo!(),
                // other
                DaemonTcp::Close(TcpClose { connection_id }) => {
                    // remove from mapping by connection
                    self.mappings_by_connection.remove(&connection_id);
                    // remove from task txs - task will be notified when tx dropped
                    self.task_txs.remove(&connection_id);
                }
                DaemonTcp::SubscribeResult(result) => match result {
                    Ok(_) => Ok(()),
                    Err(error) => Err(PortForwardError::SubscriptionError(error)),
                }?,
            },
            DaemonMessage::LogMessage(log_message) => match log_message.level {
                LogLevel::Warn => tracing::warn!("agent log: {}", log_message.message),
                LogLevel::Error => tracing::error!("agent log: {}", log_message.message),
            },
            DaemonMessage::Close(error) => {
                return Err(PortForwardError::AgentError(error));
            }
            DaemonMessage::Pong if self.waiting_for_pong => {
                self.waiting_for_pong = false;
            }
            other => {
                // includes unexepcted DaemonMessage::Pong
                return Err(PortForwardError::AgentError(format!(
                    "unexpected message from agent: {other:?}"
                )));
            }
        }

        Ok(())
    }

    #[tracing::instrument(level = Level::TRACE, skip(self), err)]
    async fn handle_msg_from_task(
        &mut self,
        message: PortForwardMessage,
    ) -> Result<(), PortForwardError> {
        match message {
            PortForwardMessage::Lookup(_, _, _) => todo!(),
            PortForwardMessage::Connect(_, _) => todo!(),
            PortForwardMessage::Send(_, _) => todo!(),
            PortForwardMessage::Close(_, _) => todo!(),
        }
    }
}

/// Used by tasks for individual forwarding connections to send instructions to [`PortForwarder`]'s
/// main loop.
#[derive(Debug)]
enum PortForwardMessage {
    /// A request to perform lookup on the given hostname at the remote peer.
    /// Sent by the task only after receiving first batch of data from the user.
    /// The task waits for [`SocketAddr`] on the other end of the [`oneshot`] channel.
    Lookup(SocketAddr, String, oneshot::Sender<IpAddr>),

    /// A request to make outgoing connection to the remote peer.
    /// Sent by the task only after receiving first batch of data from the user and after hostname
    /// resolution (if applicable). The task waits for [`ConnectionId`] on the other end of the
    /// [`oneshot`] channel.
    Connect(ResolvedPortMapping, oneshot::Sender<ConnectionId>),

    /// Data received from the user in the connection with the given id.
    Send(ConnectionId, Vec<u8>),

    /// A request to close the remote connection with the given id, if it exists, and the local
    /// socket.
    Close(SocketAddr, Option<ConnectionId>),
}

#[derive(Clone, Debug)]
enum TaskPortMapping {
    AddrPort(AddrPortMapping),
    PortOnly(PortOnlyMapping),
}

struct LocalConnectionTask {
    read_stream: ReaderStream<OwnedReadHalf>,
    write: OwnedWriteHalf,
    port_mapping: TaskPortMapping,
    task_internal_tx: Sender<PortForwardMessage>,
    response_rx: Receiver<Vec<u8>>,
}

impl LocalConnectionTask {
    pub fn new(
        stream: TcpStream,
        local_socket: SocketAddr,
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
            read_stream,
            write,
            port_mapping: TaskPortMapping::AddrPort(port_mapping),
            task_internal_tx,
            response_rx,
        }
    }

    pub async fn run(&mut self) -> Result<(), PortForwardError> {
        let TaskPortMapping::AddrPort(port_mapping) = self.port_mapping.clone() else {
            unreachable!("port_mapping type assured in new() function")
        };
        let (id_oneshot_tx, id_oneshot_rx) = oneshot::channel::<ConnectionId>();
        let (dns_oneshot_tx, dns_oneshot_rx) = oneshot::channel::<IpAddr>();

        // lazy connection: wait until data starts
        let first = match self.read_stream.next().await {
            Some(Ok(data)) => data,
            Some(Err(error)) => return Err(PortForwardError::TcpListenerError(error)),
            None => {
                // stream ended without sending data
                let _ = self
                    .task_internal_tx
                    .send(PortForwardMessage::Close(port_mapping.local, None))
                    .await;
                return Ok(());
            }
        };

        let (resolved_ip, port): (IpAddr, u16) = match &port_mapping.remote {
            (RemoteAddr::Ip(ip), port) => (IpAddr::V4(*ip), *port),
            (RemoteAddr::Hostname(hostname), port) => {
                match self
                    .task_internal_tx
                    .send(PortForwardMessage::Lookup(
                        port_mapping.local,
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
                            .send(PortForwardMessage::Close(port_mapping.local, None))
                            .await;
                        return Ok(());
                    }
                }
            }
        };
        let resolved_remote = SocketAddr::new(resolved_ip, port);
        let resolved_mapping = ResolvedPortMapping {
            local: port_mapping.local,
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
                    .send(PortForwardMessage::Close(port_mapping.local, None))
                    .await;
                return Ok(());
            }
        };
        match self
            .task_internal_tx
            .send(PortForwardMessage::Send(connection_id, first.into()))
            .await
        {
            Ok(_) => (),
            Err(error) => {
                tracing::warn!("failed to send data to main loop: {error}");
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
                            port_mapping = ?port_mapping,
                            "local connection failed",
                        );
                        break Ok(());
                    },
                    None => {
                        break Ok(());
                    },
                },

                message = self.response_rx.recv() => match message {
                    Some(message) => {
                        match self.write.write_all(message.as_ref()).await {
                            Ok(_) => continue,
                            Err(error) => {
                                tracing::error!(
                                    %error,
                                    port_mapping = ?port_mapping,
                                    "local connection failed",
                                );
                                break Ok(());
                            },
                        }
                    },
                    None => break Ok(()),
                }
            }
        };

        let _ = self
            .task_internal_tx
            .send(PortForwardMessage::Close(
                port_mapping.local,
                Some(connection_id),
            ))
            .await;
        result
    }

    pub fn new_reverse(
        stream: TcpStream,
        local_port: LocalPort,
        remote_port: RemotePort,
        task_internal_tx: Sender<PortForwardMessage>,
        response_rx: Receiver<Vec<u8>>,
    ) -> Self {
        let (read, write) = stream.into_split();
        let read_stream = ReaderStream::with_capacity(read, 64 * 1024);
        let port_mapping = PortOnlyMapping {
            local: local_port,
            remote: remote_port,
        };
        Self {
            read_stream,
            write,
            port_mapping: TaskPortMapping::PortOnly(port_mapping),
            task_internal_tx,
            response_rx,
        }
    }

    pub async fn run_reverse(&mut self) -> Result<(), PortForwardError> {
        let TaskPortMapping::PortOnly(_port_mapping) = self.port_mapping.clone() else {
            unreachable!("port_mapping type assured in new() function")
        };
        todo!()
    }
}

#[derive(Debug, Error)]
pub enum PortForwardError {
    // setup errors
    #[error("wrong combination of arguments used: {0}")]
    ArgsError(String),

    #[error("multiple port forwarding mappings found for local address `{0}`")]
    PortMapSetupError(SocketAddr),

    #[error("multiple port forwarding mappings found for desination port `{0:?}`")]
    ReversePortMapSetupError(RemotePort),

    #[error("no port forwarding mappings were provided")]
    NoMappingsError(),

    // running errors
    #[error("agent closed connection with error: `{0}`")]
    AgentError(String),

    #[error("connection with the agent failed")]
    AgentConnectionFailed,

    #[error("connection with a task failed")]
    TaskConnectionFailed,

    #[error("failed to send Ping to agent: `{0}`")]
    PingError(String),

    #[error("TcpListener operation failed with error: `{0}`")]
    TcpListenerError(std::io::Error),

    #[error("TcpStream operation failed with error: `{0}`")]
    TcpStreamError(std::io::Error),

    #[error("no destination address found for local address `{0}`")]
    SocketMappingNotFound(SocketAddr),

    #[error("no task for socket {0} ready to receive connection ID: `{1}`")]
    ReadyTaskNotFound(SocketAddr, ConnectionId),

    #[error("no task ready to receive resolved ip: `{0}`")]
    LookupReqNotFound(IpAddr),

    #[error("failed to establish connection with remote process: `{0}`")]
    ConnectionError(String),

    #[error("failed to subscribe to remote port: `{0}`")]
    SubscriptionError(ResponseError),
}

impl From<mpsc::error::SendError<ClientMessage>> for PortForwardError {
    fn from(_: mpsc::error::SendError<ClientMessage>) -> Self {
        Self::AgentConnectionFailed
    }
}

impl From<mpsc::error::SendError<Vec<u8>>> for PortForwardError {
    fn from(_: mpsc::error::SendError<Vec<u8>>) -> Self {
        Self::TaskConnectionFailed
    }
}

#[cfg(test)]
mod test {
    use std::net::{IpAddr, SocketAddr};

    use mirrord_protocol::{
        outgoing::{
            tcp::{DaemonTcpOutgoing, LayerTcpOutgoing},
            DaemonConnect, DaemonRead, LayerConnect, LayerWrite, SocketAddress,
        },
        ClientMessage, DaemonMessage,
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        sync::mpsc,
    };

    use crate::{
        connection::AgentConnection, port_forward::PortForwarder, AddrPortMapping, RemoteAddr,
    };

    #[tokio::test]
    async fn test_port_forwarding() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local_destination = listener.local_addr().unwrap();
        drop(listener);

        let (daemon_msg_tx, daemon_msg_rx) = mpsc::channel::<DaemonMessage>(12);
        let (client_msg_tx, mut client_msg_rx) = mpsc::channel::<ClientMessage>(12);

        let agent_connection = AgentConnection {
            sender: client_msg_tx,
            receiver: daemon_msg_rx,
        };
        let remote_destination = (RemoteAddr::Ip("152.37.40.40".parse().unwrap()), 3038);
        let parsed_mappings = vec![AddrPortMapping {
            local: local_destination,
            remote: remote_destination,
        }];

        tokio::spawn(async move {
            let mut port_forwarder = PortForwarder::new(agent_connection, parsed_mappings)
                .await
                .unwrap();
            port_forwarder.run().await.unwrap()
        });

        // expect handshake procedure
        let expected = Some(ClientMessage::SwitchProtocolVersion(
            mirrord_protocol::VERSION.clone(),
        ));
        assert_eq!(client_msg_rx.recv().await, expected);
        daemon_msg_tx
            .send(DaemonMessage::SwitchProtocolVersionResponse(
                mirrord_protocol::VERSION.clone(),
            ))
            .await
            .unwrap();
        let expected = Some(ClientMessage::ReadyForLogs);
        assert_eq!(client_msg_rx.recv().await, expected);

        // send data to socket
        let mut stream = TcpStream::connect(local_destination).await.unwrap();
        stream.write_all(b"data-my-beloved").await.unwrap();

        // expect Connect on client_msg_rx
        let remote_address = SocketAddress::Ip("152.37.40.40:3038".parse().unwrap());
        let expected = ClientMessage::TcpOutgoing(LayerTcpOutgoing::Connect(LayerConnect {
            remote_address: remote_address.clone(),
        }));
        let message = match client_msg_rx.recv().await.ok_or(0).unwrap() {
            ClientMessage::Ping => client_msg_rx.recv().await.ok_or(0).unwrap(),
            other => other,
        };
        assert_eq!(message, expected);

        // reply with successful on daemon_msg_tx
        daemon_msg_tx
            .send(DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Connect(Ok(
                DaemonConnect {
                    connection_id: 1,
                    remote_address: remote_address.clone(),
                    local_address: remote_address,
                },
            ))))
            .await
            .unwrap();

        let expected = ClientMessage::TcpOutgoing(LayerTcpOutgoing::Write(LayerWrite {
            connection_id: 1,
            bytes: b"data-my-beloved".to_vec(),
        }));
        let message = match client_msg_rx.recv().await.ok_or(0).unwrap() {
            ClientMessage::Ping => client_msg_rx.recv().await.ok_or(0).unwrap(),
            other => other,
        };
        assert_eq!(message, expected);

        // send response data from agent on daemon_msg_tx
        daemon_msg_tx
            .send(DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Read(Ok(
                DaemonRead {
                    connection_id: 1,
                    bytes: b"reply-my-beloved".to_vec(),
                },
            ))))
            .await
            .unwrap();

        // check data arrives at local
        let mut buf = [0; 16];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(buf, b"reply-my-beloved".as_ref());
    }

    #[tokio::test]
    async fn test_multiple_mappings_forwarding() {
        let remote_destination_1 = (RemoteAddr::Ip("152.37.40.40".parse().unwrap()), 1018);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local_destination_1 = listener.local_addr().unwrap();
        drop(listener);

        let remote_destination_2 = (RemoteAddr::Ip("152.37.40.40".parse().unwrap()), 2028);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local_destination_2 = listener.local_addr().unwrap();
        drop(listener);

        let (daemon_msg_tx, daemon_msg_rx) = mpsc::channel::<DaemonMessage>(12);
        let (client_msg_tx, mut client_msg_rx) = mpsc::channel::<ClientMessage>(12);

        let agent_connection = AgentConnection {
            sender: client_msg_tx,
            receiver: daemon_msg_rx,
        };
        let parsed_mappings = vec![
            AddrPortMapping {
                local: local_destination_1,
                remote: remote_destination_1.clone(),
            },
            AddrPortMapping {
                local: local_destination_2,
                remote: remote_destination_2.clone(),
            },
        ];

        tokio::spawn(async move {
            let mut port_forwarder = PortForwarder::new(agent_connection, parsed_mappings)
                .await
                .unwrap();
            port_forwarder.run().await.unwrap()
        });

        // expect handshake procedure
        let expected = Some(ClientMessage::SwitchProtocolVersion(
            mirrord_protocol::VERSION.clone(),
        ));
        assert_eq!(client_msg_rx.recv().await, expected);
        daemon_msg_tx
            .send(DaemonMessage::SwitchProtocolVersionResponse(
                mirrord_protocol::VERSION.clone(),
            ))
            .await
            .unwrap();
        let expected = Some(ClientMessage::ReadyForLogs);
        assert_eq!(client_msg_rx.recv().await, expected);

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
        let message = match client_msg_rx.recv().await.ok_or(0).unwrap() {
            ClientMessage::Ping => client_msg_rx.recv().await.ok_or(0).unwrap(),
            other => other,
        };
        assert_eq!(message, expected);

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
        let message = match client_msg_rx.recv().await.ok_or(0).unwrap() {
            ClientMessage::Ping => client_msg_rx.recv().await.ok_or(0).unwrap(),
            other => other,
        };
        assert_eq!(message, expected);

        // reply with successful on each daemon_msg_tx
        daemon_msg_tx
            .send(DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Connect(Ok(
                DaemonConnect {
                    connection_id: 1,
                    remote_address: remote_address_1.clone(),
                    local_address: remote_address_1,
                },
            ))))
            .await
            .unwrap();
        daemon_msg_tx
            .send(DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Connect(Ok(
                DaemonConnect {
                    connection_id: 2,
                    remote_address: remote_address_2.clone(),
                    local_address: remote_address_2,
                },
            ))))
            .await
            .unwrap();

        // expect data to be received
        let expected = ClientMessage::TcpOutgoing(LayerTcpOutgoing::Write(LayerWrite {
            connection_id: 1,
            bytes: b"data-from-1".to_vec(),
        }));
        let message = match client_msg_rx.recv().await.ok_or(0).unwrap() {
            ClientMessage::Ping => client_msg_rx.recv().await.ok_or(0).unwrap(),
            other => other,
        };
        assert_eq!(message, expected);

        let expected = ClientMessage::TcpOutgoing(LayerTcpOutgoing::Write(LayerWrite {
            connection_id: 2,
            bytes: b"data-from-2".to_vec(),
        }));
        let message = match client_msg_rx.recv().await.ok_or(0).unwrap() {
            ClientMessage::Ping => client_msg_rx.recv().await.ok_or(0).unwrap(),
            other => other,
        };
        assert_eq!(message, expected);

        // send each data response from agent on daemon_msg_tx
        daemon_msg_tx
            .send(DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Read(Ok(
                DaemonRead {
                    connection_id: 1,
                    bytes: b"reply-to-1".to_vec(),
                },
            ))))
            .await
            .unwrap();
        daemon_msg_tx
            .send(DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Read(Ok(
                DaemonRead {
                    connection_id: 2,
                    bytes: b"reply-to-2".to_vec(),
                },
            ))))
            .await
            .unwrap();

        // check data arrives at each local addr
        let mut buf = [0; 10];
        stream_1.read_exact(&mut buf).await.unwrap();
        assert_eq!(buf, b"reply-to-1".as_ref());
        let mut buf = [0; 10];
        stream_2.read_exact(&mut buf).await.unwrap();
        assert_eq!(buf, b"reply-to-2".as_ref());
    }
}
