use std::{
    collections::{HashMap, HashSet, VecDeque},
    net::{IpAddr, SocketAddr},
    time::{Duration, Instant},
};

use futures::StreamExt;
use mirrord_config::feature::network::incoming::{
    http_filter::{HttpFilterConfig, InnerFilter},
    IncomingConfig,
};
use mirrord_intproxy::{
    background_tasks::{BackgroundTasks, TaskError, TaskSender, TaskUpdate},
    error::IntProxyError,
    main_tasks::{MainTaskId, ProxyMessage, ToLayer},
    proxies::incoming::{
        port_subscription_ext::PortSubscriptionExt, IncomingProxy, IncomingProxyError,
        IncomingProxyMessage,
    },
};
use mirrord_intproxy_protocol::{
    IncomingRequest, IncomingResponse, LayerId, PortSubscribe, PortSubscription,
    ProxyToLayerMessage,
};
use mirrord_protocol::{
    dns::{DnsLookup, GetAddrInfoRequest, GetAddrInfoResponse, LookupRecord},
    outgoing::{
        tcp::{DaemonTcpOutgoing, LayerTcpOutgoing},
        LayerClose, LayerConnect, LayerWrite, SocketAddress,
    },
    tcp::{Filter, HttpFilter, LayerTcp, LayerTcpSteal, StealType},
    ClientMessage, ConnectionId, DaemonMessage, LogLevel, Port, CLIENT_READY_FOR_LOGS,
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

use crate::{connection::AgentConnection, AddrPortMapping, LocalPort, RemoteAddr, RemotePort};

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
                LogLevel::Info => tracing::info!("agent log: {}", log_message.message),
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
            PortForwardMessage::Close(port_mapping, connection_id) => {
                self.task_txs.remove(&port_mapping.local);
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
    /// details for traffic mirroring or stealing
    incoming_mode: IncomingMode,
    /// communicates with the agent (only TCP supported).
    agent_connection: AgentConnection,
    /// associates destination ports with local ports.
    mappings: HashMap<RemotePort, LocalPort>,
    /// background task (uses IncomingProxy to communicate with layer)
    background_tasks: BackgroundTasks<MainTaskId, ProxyMessage, IntProxyError>,
    /// incoming proxy background task tx
    incoming_proxy: TaskSender<IncomingProxy>,

    /// true if Ping has been sent to agent.
    waiting_for_pong: bool,
    ping_pong_timeout: Instant,
}

impl ReversePortForwarder {
    pub(crate) async fn new(
        agent_connection: AgentConnection,
        mappings: HashMap<RemotePort, LocalPort>,
        network_config: IncomingConfig,
    ) -> Result<Self, PortForwardError> {
        // setup IncomingProxy
        let mut background_tasks: BackgroundTasks<MainTaskId, ProxyMessage, IntProxyError> =
            Default::default();
        let incoming =
            background_tasks.register(IncomingProxy::default(), MainTaskId::IncomingProxy, 512);
        // construct IncomingMode from config file
        let incoming_mode = IncomingMode::new(&network_config);
        for (i, (&remote, &local)) in mappings.iter().enumerate() {
            // send subscription to incoming proxy
            let subscription = incoming_mode.subscription(remote);
            let message_id = i as u64;
            let layer_id = LayerId(1);
            let req = IncomingRequest::PortSubscribe(PortSubscribe {
                listening_on: format!("127.0.0.1:{local}")
                    .parse()
                    .expect("Error parsing socket address"),
                subscription,
            });
            incoming
                .send(IncomingProxyMessage::LayerRequest(
                    message_id, layer_id, req,
                ))
                .await;
        }

        Ok(Self {
            incoming_mode,
            agent_connection,
            mappings,
            background_tasks,
            incoming_proxy: incoming,
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
            let subscription = self.incoming_mode.subscription(*remote_port);
            let msg = subscription.agent_subscribe();
            self.agent_connection.sender.send(msg).await?
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
                    Some(message) => {
                        self.handle_msg_from_agent(message).await?},
                    None => {
                        break Err(PortForwardError::AgentError("unexpected end of connection with agent".into()));
                    },
                },

                Some((task_id, update)) = self.background_tasks.next() => {
                    self.handle_msg_from_local(task_id, update).await?
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
    async fn handle_msg_from_local(
        &mut self,
        task_id: MainTaskId,
        update: TaskUpdate<ProxyMessage, IntProxyError>,
    ) -> Result<(), PortForwardError> {
        match (task_id, update) {
            (MainTaskId::IncomingProxy, TaskUpdate::Message(message)) => match message {
                ProxyMessage::ToAgent(message) => {
                    if matches!(
                        message,
                        ClientMessage::TcpSteal(LayerTcpSteal::PortSubscribe(_))
                            | ClientMessage::Tcp(LayerTcp::PortSubscribe(_))
                    ) {
                        // suppress additional subscription requests
                        return Ok(());
                    }
                    self.agent_connection.sender.send(message).await?;
                }
                ProxyMessage::ToLayer(ToLayer {
                    message: ProxyToLayerMessage::Incoming(IncomingResponse::PortSubscribe(res)),
                    ..
                }) => {
                    if let Err(error) = res {
                        return Err(PortForwardError::from(IntProxyError::from(
                            IncomingProxyError::SubscriptionFailed(error),
                        )));
                    }
                }
                other => {
                    tracing::debug!(
                        "unexpected message type from background task (message ignored): {other:?}"
                    )
                }
            },
            (MainTaskId::IncomingProxy, TaskUpdate::Finished(result)) => match result {
                Ok(()) => {
                    tracing::error!("incoming proxy task finished unexpectedly");
                    return Err(IntProxyError::TaskExit(task_id).into());
                }
                Err(TaskError::Error(e)) => {
                    tracing::error!("incoming proxy task failed: {e}");
                    return Err(e.into());
                }
                Err(TaskError::Panic) => {
                    tracing::error!("incoming proxy task panicked");
                    return Err(IntProxyError::TaskPanic(task_id).into());
                }
            },
            _ => unreachable!("other task types are never used in port forwarding"),
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
    Close(AddrPortMapping, Option<ConnectionId>),
}

struct LocalConnectionTask {
    /// read half of the TcpStream connected to the local port, wrapped in a stream
    read_stream: ReaderStream<OwnedReadHalf>,
    /// write half of the TcpStream connected to the local port
    write: OwnedWriteHalf,
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
            port_mapping,
            task_internal_tx,
            data_rx: response_rx,
        }
    }

    pub async fn run(&mut self) -> Result<(), PortForwardError> {
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
                    .send(PortForwardMessage::Close(self.port_mapping.clone(), None))
                    .await;
                return Ok(());
            }
        };

        let (resolved_ip, port): (IpAddr, u16) = match &self.port_mapping.remote {
            (RemoteAddr::Ip(ip), port) => (IpAddr::V4(*ip), *port),
            (RemoteAddr::Hostname(hostname), port) => {
                match self
                    .task_internal_tx
                    .send(PortForwardMessage::Lookup(
                        self.port_mapping.local,
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
                            .send(PortForwardMessage::Close(self.port_mapping.clone(), None))
                            .await;
                        return Ok(());
                    }
                }
            }
        };
        let resolved_remote = SocketAddr::new(resolved_ip, port);
        let resolved_mapping = ResolvedPortMapping {
            local: self.port_mapping.local,
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
                    .send(PortForwardMessage::Close(self.port_mapping.clone(), None))
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
                    Some(message) => {
                        match self.write.write_all(message.as_ref()).await {
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
                    },
                    None => break Ok(()),
                }
            }
        };

        let _ = self
            .task_internal_tx
            .send(PortForwardMessage::Close(
                self.port_mapping.clone(),
                Some(connection_id),
            ))
            .await;
        result
    }
}

/// Structs from mirrord_layer
/// HTTP filter used by the layer with the `steal` feature.
#[derive(Debug)]
pub enum StealHttpFilter {
    /// No filter.
    None,
    /// More recent filter (header or path).
    Filter(HttpFilter),
}

/// Settings for handling HTTP with the `steal` feature.
#[derive(Debug)]
struct StealHttpSettings {
    /// The HTTP filter to use.
    pub filter: StealHttpFilter,
    /// Ports to filter HTTP on.
    pub ports: HashSet<Port>,
}

/// Operation mode for the `incoming` feature.
#[derive(Debug)]
enum IncomingMode {
    /// The agent sends data to both the user application and the remote target.
    /// Data coming from the layer is discarded.
    Mirror,
    /// The agent sends data only to the user application.
    /// Data coming from the layer is sent to the agent.
    Steal(StealHttpSettings),
}

impl IncomingMode {
    /// Creates a new instance from the given [`IncomingConfig`].
    fn new(config: &IncomingConfig) -> Self {
        if !config.is_steal() {
            return Self::Mirror;
        }

        let http_filter_config = &config.http_filter;

        let ports = { http_filter_config.ports.iter().copied().collect() };

        // Matching all fields to make this check future-proof.
        let filter = match http_filter_config {
            HttpFilterConfig {
                path_filter: Some(path),
                header_filter: None,
                all_of: None,
                any_of: None,
                ports: _ports,
            } => StealHttpFilter::Filter(HttpFilter::Path(
                Filter::new(path.into()).expect("invalid filter expression"),
            )),

            HttpFilterConfig {
                path_filter: None,
                header_filter: Some(header),
                all_of: None,
                any_of: None,
                ports: _ports,
            } => StealHttpFilter::Filter(HttpFilter::Header(
                Filter::new(header.into()).expect("invalid filter expression"),
            )),

            HttpFilterConfig {
                path_filter: None,
                header_filter: None,
                all_of: Some(filters),
                any_of: None,
                ports: _ports,
            } => StealHttpFilter::Filter(Self::make_composite_filter(true, filters)),

            HttpFilterConfig {
                path_filter: None,
                header_filter: None,
                all_of: None,
                any_of: Some(filters),
                ports: _ports,
            } => StealHttpFilter::Filter(Self::make_composite_filter(false, filters)),

            HttpFilterConfig {
                path_filter: None,
                header_filter: None,
                all_of: None,
                any_of: None,
                ports: _ports,
            } => StealHttpFilter::None,

            _ => panic!("multiple HTTP filters specified, this is a bug"),
        };

        Self::Steal(StealHttpSettings { filter, ports })
    }

    fn make_composite_filter(all: bool, filters: &[InnerFilter]) -> HttpFilter {
        let filters = filters
            .iter()
            .map(|filter| match filter {
                InnerFilter::Path { path } => {
                    HttpFilter::Path(Filter::new(path.clone()).expect("invalid filter expression"))
                }
                InnerFilter::Header { header } => HttpFilter::Header(
                    Filter::new(header.clone()).expect("invalid filter expression"),
                ),
            })
            .collect();

        HttpFilter::Composite { all, filters }
    }

    /// Returns [`PortSubscription`] request to be used for the given port.
    fn subscription(&self, port: Port) -> PortSubscription {
        let Self::Steal(steal) = self else {
            return PortSubscription::Mirror(port);
        };

        let steal_type = match &steal.filter {
            _ if !steal.ports.contains(&port) => StealType::All(port),
            StealHttpFilter::None => StealType::All(port),
            StealHttpFilter::Filter(filter) => StealType::FilteredHttpEx(port, filter.clone()),
        };

        PortSubscription::Steal(steal_type)
    }
}

#[derive(Debug, Error)]
pub enum PortForwardError {
    #[error("multiple port forwarding mappings found for local address `{0}`")]
    PortMapSetupError(SocketAddr),

    #[error("multiple port forwarding mappings found for desination port `{0:?}`")]
    ReversePortMapSetupError(RemotePort),

    // running errors
    #[error("agent closed connection with error: `{0}`")]
    AgentError(String),

    #[error("connection with the agent failed")]
    AgentConnectionFailed,

    #[error("error from Incoming Proxy task")]
    IncomingProxyError(IntProxyError),

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

impl From<IntProxyError> for PortForwardError {
    fn from(value: IntProxyError) -> Self {
        Self::IncomingProxyError(value)
    }
}

#[cfg(test)]
mod test {
    use std::{
        collections::HashMap,
        net::{IpAddr, Ipv4Addr, SocketAddr},
        time::Duration,
    };

    use mirrord_config::feature::network::incoming::{IncomingConfig, IncomingMode};
    use mirrord_protocol::{
        outgoing::{
            tcp::{DaemonTcpOutgoing, LayerTcpOutgoing},
            DaemonConnect, DaemonRead, LayerConnect, LayerWrite, SocketAddress,
        },
        tcp::{
            DaemonTcp, Filter, HttpRequest, HttpResponse, InternalHttpRequest,
            InternalHttpResponse, LayerTcp, LayerTcpSteal, NewTcpConnection, StealType, TcpClose,
            TcpData,
        },
        ClientMessage, DaemonMessage,
    };
    use reqwest::{header::HeaderMap, Method, StatusCode, Version};
    use rstest::rstest;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        sync::mpsc,
    };

    use crate::{
        connection::AgentConnection,
        port_forward::{PortForwarder, ReversePortForwarder},
        RemoteAddr,
    };

    #[tokio::test]
    async fn single_port_forwarding() {
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
        let mappings = HashMap::from([(local_destination, remote_destination.clone())]);

        tokio::spawn(async move {
            let mut port_forwarder = PortForwarder::new(agent_connection, mappings)
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
    async fn multiple_mappings_port_forwarding() {
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
        let mappings = HashMap::from([
            (local_destination_1, remote_destination_1.clone()),
            (local_destination_2, remote_destination_2.clone()),
        ]);

        tokio::spawn(async move {
            let mut port_forwarder = PortForwarder::new(agent_connection, mappings)
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

    #[rstest]
    #[tokio::test]
    #[timeout(Duration::from_secs(5))]
    async fn reverse_port_forwarding_mirror() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local_destination = listener.local_addr().unwrap();

        let (daemon_msg_tx, daemon_msg_rx) = mpsc::channel::<DaemonMessage>(12);
        let (client_msg_tx, mut client_msg_rx) = mpsc::channel::<ClientMessage>(12);

        let agent_connection = AgentConnection {
            sender: client_msg_tx,
            receiver: daemon_msg_rx,
        };
        let remote_address = IpAddr::from("152.37.40.40".parse::<Ipv4Addr>().unwrap());
        let destination_port = 3038;
        let mappings = HashMap::from([(destination_port, local_destination.port())]);
        let network_config = IncomingConfig::default();

        tokio::spawn(async move {
            let mut port_forwarder =
                ReversePortForwarder::new(agent_connection, mappings, network_config)
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

        // expect port subscription for remote port and send subscribe result
        let expected = Some(ClientMessage::Tcp(LayerTcp::PortSubscribe(
            destination_port,
        )));
        assert_eq!(client_msg_rx.recv().await, expected);
        daemon_msg_tx
            .send(DaemonMessage::Tcp(DaemonTcp::SubscribeResult(Ok(
                destination_port,
            ))))
            .await
            .unwrap();

        // send new connection from agent and some data
        daemon_msg_tx
            .send(DaemonMessage::Tcp(DaemonTcp::NewConnection(
                NewTcpConnection {
                    connection_id: 1,
                    remote_address,
                    destination_port,
                    source_port: local_destination.port(),
                    local_address: local_destination.ip(),
                },
            )))
            .await
            .unwrap();
        let mut stream = listener.accept().await.unwrap().0;

        daemon_msg_tx
            .send(DaemonMessage::Tcp(DaemonTcp::Data(TcpData {
                connection_id: 1,
                bytes: b"data-my-beloved".to_vec(),
            })))
            .await
            .unwrap();

        // check data arrives at local
        let mut buf = [0; 15];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(buf, b"data-my-beloved".as_ref());

        // ensure graceful behaviour on close
        daemon_msg_tx
            .send(DaemonMessage::Tcp(DaemonTcp::Close(TcpClose {
                connection_id: 1,
            })))
            .await
            .unwrap();
    }

    #[rstest]
    #[tokio::test]
    #[timeout(Duration::from_secs(5))]
    async fn reverse_port_forwarding_steal() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local_destination = listener.local_addr().unwrap();

        let (daemon_msg_tx, daemon_msg_rx) = mpsc::channel::<DaemonMessage>(12);
        let (client_msg_tx, mut client_msg_rx) = mpsc::channel::<ClientMessage>(12);

        let agent_connection = AgentConnection {
            sender: client_msg_tx,
            receiver: daemon_msg_rx,
        };
        let remote_address = IpAddr::from("152.37.40.40".parse::<Ipv4Addr>().unwrap());
        let destination_port = 3038;
        let mappings = HashMap::from([(destination_port, local_destination.port())]);
        let network_config = IncomingConfig {
            mode: IncomingMode::Steal,
            ..Default::default()
        };

        tokio::spawn(async move {
            let mut port_forwarder =
                ReversePortForwarder::new(agent_connection, mappings, network_config)
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

        // expect port subscription for remote port and send subscribe result
        let expected = Some(ClientMessage::TcpSteal(LayerTcpSteal::PortSubscribe(
            StealType::All(destination_port),
        )));
        assert_eq!(client_msg_rx.recv().await, expected);
        daemon_msg_tx
            .send(DaemonMessage::Tcp(DaemonTcp::SubscribeResult(Ok(
                destination_port,
            ))))
            .await
            .unwrap();

        // send new connection from agent and some data
        daemon_msg_tx
            .send(DaemonMessage::Tcp(DaemonTcp::NewConnection(
                NewTcpConnection {
                    connection_id: 1,
                    remote_address,
                    destination_port,
                    source_port: local_destination.port(),
                    local_address: local_destination.ip(),
                },
            )))
            .await
            .unwrap();
        let mut stream = listener.accept().await.unwrap().0;

        daemon_msg_tx
            .send(DaemonMessage::TcpSteal(DaemonTcp::Data(TcpData {
                connection_id: 1,
                bytes: b"data-my-beloved".to_vec(),
            })))
            .await
            .unwrap();

        // check data arrives at local
        let mut buf = [0; 15];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(buf, b"data-my-beloved".as_ref());

        // check for response from local
        stream.write_all(b"reply-my-beloved").await.unwrap();
        let message = match client_msg_rx.recv().await.ok_or(0).unwrap() {
            ClientMessage::Ping => client_msg_rx.recv().await.ok_or(0).unwrap(),
            other => other,
        };
        assert_eq!(
            message,
            ClientMessage::TcpSteal(LayerTcpSteal::Data(TcpData {
                connection_id: 1,
                bytes: b"reply-my-beloved".to_vec()
            }))
        );

        // ensure graceful behaviour on close
        daemon_msg_tx
            .send(DaemonMessage::Tcp(DaemonTcp::Close(TcpClose {
                connection_id: 1,
            })))
            .await
            .unwrap();
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

        let (daemon_msg_tx, daemon_msg_rx) = mpsc::channel::<DaemonMessage>(12);
        let (client_msg_tx, mut client_msg_rx) = mpsc::channel::<ClientMessage>(12);

        let agent_connection = AgentConnection {
            sender: client_msg_tx,
            receiver: daemon_msg_rx,
        };
        let remote_address = IpAddr::from("152.37.40.40".parse::<Ipv4Addr>().unwrap());
        let destination_port_1 = 3038;
        let destination_port_2 = 4048;
        let mappings = HashMap::from([
            (destination_port_1, local_destination_1.port()),
            (destination_port_2, local_destination_2.port()),
        ]);
        let network_config = IncomingConfig::default();

        tokio::spawn(async move {
            let mut port_forwarder =
                ReversePortForwarder::new(agent_connection, mappings, network_config)
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

        // expect port subscription for each remote port and send subscribe result
        // matches! used because order may be random
        for _ in 0..2 {
            let message = match client_msg_rx.recv().await.ok_or(0).unwrap() {
                ClientMessage::Ping => client_msg_rx.recv().await.ok_or(0).unwrap(),
                other => other,
            };
            assert!(
                matches!(message, ClientMessage::Tcp(LayerTcp::PortSubscribe(_))),
                "expected ClientMessage::Tcp(LayerTcp::PortSubscribe(_), received {message:?}"
            );
        }

        daemon_msg_tx
            .send(DaemonMessage::Tcp(DaemonTcp::SubscribeResult(Ok(
                destination_port_1,
            ))))
            .await
            .unwrap();
        daemon_msg_tx
            .send(DaemonMessage::Tcp(DaemonTcp::SubscribeResult(Ok(
                destination_port_2,
            ))))
            .await
            .unwrap();

        // send new connections from agent and some data
        daemon_msg_tx
            .send(DaemonMessage::Tcp(DaemonTcp::NewConnection(
                NewTcpConnection {
                    connection_id: 1,
                    remote_address,
                    destination_port: destination_port_1,
                    source_port: local_destination_1.port(),
                    local_address: local_destination_1.ip(),
                },
            )))
            .await
            .unwrap();
        let mut stream_1 = listener_1.accept().await.unwrap().0;

        daemon_msg_tx
            .send(DaemonMessage::Tcp(DaemonTcp::NewConnection(
                NewTcpConnection {
                    connection_id: 2,
                    remote_address,
                    destination_port: destination_port_2,
                    source_port: local_destination_2.port(),
                    local_address: local_destination_2.ip(),
                },
            )))
            .await
            .unwrap();
        let mut stream_2 = listener_2.accept().await.unwrap().0;

        daemon_msg_tx
            .send(DaemonMessage::Tcp(DaemonTcp::Data(TcpData {
                connection_id: 1,
                bytes: b"connection-1-my-beloved".to_vec(),
            })))
            .await
            .unwrap();

        daemon_msg_tx
            .send(DaemonMessage::Tcp(DaemonTcp::Data(TcpData {
                connection_id: 2,
                bytes: b"connection-2-my-beloved".to_vec(),
            })))
            .await
            .unwrap();

        // check data arrives at local
        let mut buf = [0; 23];
        stream_1.read_exact(&mut buf).await.unwrap();
        assert_eq!(buf, b"connection-1-my-beloved".as_ref());

        let mut buf = [0; 23];
        stream_2.read_exact(&mut buf).await.unwrap();
        assert_eq!(buf, b"connection-2-my-beloved".as_ref());

        // ensure graceful behaviour on close
        daemon_msg_tx
            .send(DaemonMessage::Tcp(DaemonTcp::Close(TcpClose {
                connection_id: 1,
            })))
            .await
            .unwrap();

        daemon_msg_tx
            .send(DaemonMessage::Tcp(DaemonTcp::Close(TcpClose {
                connection_id: 2,
            })))
            .await
            .unwrap();
    }

    #[rstest]
    #[tokio::test]
    #[timeout(Duration::from_secs(5))]
    async fn filtered_reverse_port_forwarding() {
        // simulates filtered stealing with one port mapping
        // filters are matched in the agent but this tests Http type messages
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local_destination = listener.local_addr().unwrap();

        let (daemon_msg_tx, daemon_msg_rx) = mpsc::channel::<DaemonMessage>(12);
        let (client_msg_tx, mut client_msg_rx) = mpsc::channel::<ClientMessage>(12);

        let agent_connection = AgentConnection {
            sender: client_msg_tx,
            receiver: daemon_msg_rx,
        };
        let remote_address = IpAddr::from("152.37.40.40".parse::<Ipv4Addr>().unwrap());
        let destination_port = 8080;
        let mappings = HashMap::from([(destination_port, local_destination.port())]);
        let mut network_config = IncomingConfig {
            mode: IncomingMode::Steal,
            ..Default::default()
        };
        network_config.http_filter.header_filter = Some("header: value".to_string());

        tokio::spawn(async move {
            let mut port_forwarder =
                ReversePortForwarder::new(agent_connection, mappings, network_config)
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

        // expect port subscription for remote port and send subscribe result
        let message = match client_msg_rx.recv().await.ok_or(0).unwrap() {
            ClientMessage::Ping => client_msg_rx.recv().await.ok_or(0).unwrap(),
            other => other,
        };
        assert_eq!(
            message,
            ClientMessage::TcpSteal(LayerTcpSteal::PortSubscribe(StealType::FilteredHttpEx(
                destination_port,
                mirrord_protocol::tcp::HttpFilter::Header(
                    Filter::new("header: value".to_string()).unwrap()
                )
            ),))
        );
        daemon_msg_tx
            .send(DaemonMessage::Tcp(DaemonTcp::SubscribeResult(Ok(
                destination_port,
            ))))
            .await
            .unwrap();

        // send new connection from agent and some data
        daemon_msg_tx
            .send(DaemonMessage::TcpSteal(DaemonTcp::NewConnection(
                NewTcpConnection {
                    connection_id: 1,
                    remote_address,
                    destination_port,
                    source_port: local_destination.port(),
                    local_address: local_destination.ip(),
                },
            )))
            .await
            .unwrap();
        let mut stream = listener.accept().await.unwrap().0;

        // send data from agent with correct header
        let mut headers = HeaderMap::new();
        headers.insert("header", "value".parse().unwrap());
        let internal_request = InternalHttpRequest {
            method: Method::GET,
            uri: "https://www.rust-lang.org/install.html".parse().unwrap(),
            headers,
            version: Version::HTTP_11,
            body: vec![],
        };
        daemon_msg_tx
            .send(DaemonMessage::TcpSteal(DaemonTcp::HttpRequest(
                HttpRequest {
                    internal_request,
                    connection_id: 1,
                    request_id: 1,
                    port: local_destination.port(),
                },
            )))
            .await
            .unwrap();

        // check data is read from stream
        let mut buf = [0; 15];
        assert_eq!(buf, [0; 15]);
        stream.read_exact(&mut buf).await.unwrap();
        assert_ne!(buf, [0; 15]);

        // check for response from local
        stream
            .write_all("HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nyay".as_bytes())
            .await
            .unwrap();

        let mut headers = HeaderMap::new();
        headers.insert("content-length", "3".parse().unwrap());
        let internal_response = InternalHttpResponse {
            status: StatusCode::from_u16(200).unwrap(),
            version: Version::HTTP_11,
            headers,
            body: b"yay".to_vec(),
        };
        let expected_response =
            ClientMessage::TcpSteal(LayerTcpSteal::HttpResponse(HttpResponse {
                connection_id: 1,
                request_id: 1,
                port: local_destination.port(),
                internal_response,
            }));

        let message = match client_msg_rx.recv().await.ok_or(0).unwrap() {
            ClientMessage::Ping => client_msg_rx.recv().await.ok_or(0).unwrap(),
            other => other,
        };
        assert_eq!(message, expected_response);

        // ensure graceful behaviour on close
        daemon_msg_tx
            .send(DaemonMessage::Tcp(DaemonTcp::Close(TcpClose {
                connection_id: 1,
            })))
            .await
            .unwrap();
    }
}
