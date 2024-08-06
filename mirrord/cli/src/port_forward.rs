use std::{
    collections::HashMap,
    net::SocketAddr,
    time::{Duration, Instant},
};

use futures::future::Either;
use mirrord_protocol::{
    outgoing::{
        tcp::{DaemonTcpOutgoing, LayerTcpOutgoing},
        LayerClose, LayerConnect, SocketAddress,
    },
    ClientMessage, ConnectionId, DaemonMessage, LogLevel, CLIENT_READY_FOR_LOGS,
};
use thiserror::Error;
use tokio::{
    net::{
        tcp::{OwnedReadHalf, OwnedWriteHalf},
        TcpListener,
    },
    select,
};
use tokio_stream::{wrappers::TcpListenerStream, StreamExt, StreamMap};

use crate::{connection::AgentConnection, CliError, PortMapping};

pub struct PortForwarder {
    // communicates with the agent (only TCP supported)
    agent_connection: AgentConnection,
    // associates local ports with destination ports
    mappings: HashMap<SocketAddr, SocketAddr>,
    // accepts connections from the user app in the form of a stream
    listeners: StreamMap<SocketAddr, TcpListenerStream>,
    // the reading half of the stream received by a listener for receiving data to send to the
    rx_connections: StreamMap<SocketAddr, OwnedReadHalf>,
    // the writing half of the stream received by a listener for sending data back to the user app
    tx_connections: HashMap<SocketAddr, OwnedWriteHalf>,
    // bijective map for storing connection_ids and sockets
    sockets_ids_map: BiMap,
    // true if Ping has been sent to agent
    waiting_for_pong: bool,
    ping_pong_timeout: Instant,
}

impl PortForwarder {
    pub(crate) async fn new(
        agent_connection: AgentConnection,
        parsed_mappings: Vec<PortMapping>,
    ) -> Result<Self, PortForwardError> {
        // open tcp listener for local addrs
        let mut listeners = StreamMap::new();
        let mut mappings = HashMap::new();

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

        Ok(Self {
            agent_connection,
            mappings,
            listeners,
            rx_connections: StreamMap::new(),
            tx_connections: HashMap::new(),
            sockets_ids_map: BiMap {
                connection_ids: HashMap::new(),
                sockets: HashMap::new(),
            },
            waiting_for_pong: false,
            ping_pong_timeout: Instant::now(),
        })
    }

    pub(crate) async fn run(&mut self) -> Result<(), PortForwardError> {
        // setup agent connection
        let Ok(_) = self
            .agent_connection
            .sender
            .send(ClientMessage::SwitchProtocolVersion(
                mirrord_protocol::VERSION.clone(),
            ))
            .await
        else {
            // failed to send to agent
            return Err(PortForwardError::AgentSetupError(
                "Sending 'Switch Protocol Version' Client Message failed".into(),
            ));
        };
        match self.agent_connection.receiver.recv().await {
            Some(DaemonMessage::SwitchProtocolVersionResponse(version))
                if CLIENT_READY_FOR_LOGS.matches(&version) =>
            {
                let Ok(_) = self
                    .agent_connection
                    .sender
                    .send(ClientMessage::ReadyForLogs)
                    .await
                else {
                    return Err(PortForwardError::AgentSetupError(
                        "Sending 'Ready for Logs' Client Message failed".into(),
                    ));
                };
            }
            _ => {
                return Err(PortForwardError::AgentSetupError(
                    "Switching protocol version check failed".into(),
                ))
            }
        }

        loop {
            select! {
                _ = tokio::time::sleep_until(self.ping_pong_timeout.into()) => {
                    if self.waiting_for_pong {
                        // no pong received before timeout
                        break Err(PortForwardError::AgentError("Agent failed to respond to Ping".into()));
                    }
                    let _ = self.agent_connection.sender.send(ClientMessage::Ping).await;
                    self.waiting_for_pong = true;
                    self.ping_pong_timeout = Instant::now() + Duration::from_secs(30);
                },

                message = self.agent_connection.receiver.recv() => match message {
                    Some(message) => {
                        match message {
                            DaemonMessage::TcpOutgoing(message) => {
                                match message {
                                    DaemonTcpOutgoing::Connect(res) => {
                                        match res {
                                            Ok(connection) => {
                                                let SocketAddress::Ip(local_address) = connection.local_address else {
                                                    return Err(PortForwardError::ConnectionError("Unexpectedly received Unix address for socket during setup".into()));
                                                };
                                                self.sockets_ids_map.insert(connection.connection_id, local_address);
                                                // TODO: new connection made successfully, spawn new task to proxy data
                                                // tokio::spawn();
                                            },
                                            Err(error) => return Err(PortForwardError::ConnectionError(format!("{error}"))),
                                        }
                                    },
                                    DaemonTcpOutgoing::Read(res) => todo!(), // TODO: :)
                                    DaemonTcpOutgoing::Close(connection_id) => {
                                        if let Some(Either::Right(socket)) = self.sockets_ids_map.get(Either::Left(connection_id)) {
                                            self.sockets_ids_map.remove(Either::Right(socket));
                                            self.rx_connections.remove(&socket);
                                            self.tx_connections.remove(&socket);
                                        }
                                    },
                                }
                            },
                            DaemonMessage::LogMessage(log_message) => {
                                match log_message.level {
                                    LogLevel::Warn => tracing::warn!(log_message.message),
                                    LogLevel::Error => tracing::error!(log_message.message),
                                }
                            },
                            DaemonMessage::Pong => {
                                if !self.waiting_for_pong {
                                    // message not expected
                                    break Err(PortForwardError::AgentError("Unexpected message from Agent: DaemonMessage::Pong".into()));
                                }
                                self.waiting_for_pong = false;
                            },
                            DaemonMessage::Close(error) => {
                                break Err(PortForwardError::AgentError(error));
                            },
                            other@_ => {
                                break Err(PortForwardError::AgentError(format!("Unexpected message from Agent: {other:?}")));
                            },
                        }
                    },
                    None => {
                        tracing::trace!("None message received, connection ended with agent");
                        break Ok(());
                    },
                },

                // stream coming from the user app
                message = self.listeners.next() => match message {
                    Some((socket, Ok(stream))) => {
                        // split the stream and add to rx/ tx
                        let (read, write) = stream.into_split();
                        self.rx_connections.insert(socket, read);
                        self.tx_connections.insert(socket, write);
                        // get destination socket from mappings
                        let destination =  match self.mappings.get(&socket) {
                            Some(address) => address,
                            None => return Err(PortForwardError::SocketMappingNotFound(socket)),
                        };
                        let dest_socket = SocketAddress::Ip(*destination);
                        let Ok(_) = self.agent_connection.sender.send(ClientMessage::TcpOutgoing(LayerTcpOutgoing::Connect(LayerConnect{remote_address: dest_socket}))).await else {
                            return Err(PortForwardError::AgentSetupError("Failed to send connection request to agent".into()));
                        };
                    },
                    Some((socket, Err(error))) => {
                        // error from TcpStream
                        tracing::error!("Error occured while listening to local socket {socket}: {error}");
                        self.rx_connections.remove(&socket);
                        self.tx_connections.remove(&socket);
                        if let Some(Either::Left(connection_id)) = self.sockets_ids_map.get(Either::Right(socket)) {
                            let _ = self.agent_connection.sender.send(ClientMessage::TcpOutgoing(LayerTcpOutgoing::Close(LayerClose{ connection_id }))).await;
                        }
                        self.sockets_ids_map.remove(Either::Right(socket));
                    },
                    None => break Ok(()), // all streams ended
                }
            }
        }
    }
}

pub struct BiMap {
    // connection IDs for successful connections
    connection_ids: HashMap<SocketAddr, ConnectionId>,
    // connection IDs for successful connections
    sockets: HashMap<ConnectionId, SocketAddr>,
}

impl BiMap {
    fn socket(&self, connection_id: ConnectionId) -> Option<&SocketAddr> {
        self.sockets.get(&connection_id)
    }

    fn connection_id(&self, socket: SocketAddr) -> Option<&ConnectionId> {
        self.connection_ids.get(&socket)
    }

    /// retrieve a value from the BiMap given either value
    /// returns Some() if value was present, otherwise returns None
    pub fn get(
        &self,
        item: Either<ConnectionId, SocketAddr>,
    ) -> Option<Either<ConnectionId, SocketAddr>> {
        match item {
            Either::Left(connection_id) => self.socket(connection_id).cloned().map(Either::Right),
            Either::Right(socket) => self.connection_id(socket).cloned().map(Either::Left),
        }
    }

    /// insert a pair of values into the BiMap, returning None if successful
    /// if value(s) already present for either item, Some() is returned and contains the existing
    /// value(s)
    pub fn insert(
        &mut self,
        connection_id: ConnectionId,
        socket: SocketAddr,
    ) -> Option<(Option<SocketAddr>, Option<ConnectionId>)> {
        let res_s = self.sockets.insert(connection_id, socket);
        let res_c = self.connection_ids.insert(socket, connection_id);
        match (res_s, res_c) {
            (None, None) => return None,
            _ => return Some((res_s, res_c)),
        }
    }

    /// remove a pair of values from the BiMap given either value
    /// returns Some() if both values were present, otherwise returns None
    /// note that if None is returned but one value was present, it has been removed
    pub fn remove(&mut self, item: Either<ConnectionId, SocketAddr>) -> Option<()> {
        let (connection_id, socket) = match item {
            Either::Left(connection_id) => {
                (Some(connection_id), self.socket(connection_id).cloned())
            }
            Either::Right(socket) => (self.connection_id(socket).cloned(), Some(socket)),
        };

        let res_s = if let Some(inner) = connection_id {
            self.sockets.remove(&inner)
        } else {
            None
        };
        let res_c = if let Some(inner) = socket {
            self.connection_ids.remove(&inner)
        } else {
            None
        };
        match (res_s, res_c) {
            (Some(_), Some(_)) => return Some(()),
            _ => return None,
        }
    }
}

#[derive(Debug, Error)]
pub enum PortForwardError {
    #[error("Failed to setup connection with Agent with error: `{0}`")]
    AgentSetupError(String),

    #[error("Error occurred in the Agent: `{0}`")]
    AgentError(String),

    #[error("Multiple port forwarding mappings found for local address `{0}`")]
    PortMapSetupError(SocketAddr),

    #[error("Failed to bind TcpListener with error: `{0}`")]
    TcpListenerError(std::io::Error),

    #[error("No destination address found for local address `{0}`")]
    SocketMappingNotFound(SocketAddr),

    #[error("Failed to establish connection with remote process: `{0}`")]
    ConnectionError(String),
}

impl From<PortForwardError> for CliError {
    fn from(value: PortForwardError) -> Self {
        CliError::PortForwardingError(value)
    }
}
