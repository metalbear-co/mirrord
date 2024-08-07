use std::{
    collections::{HashMap, VecDeque},
    io::Bytes,
    net::SocketAddr,
    time::{Duration, Instant},
};

use futures::future::Either;
use mirrord_protocol::{
    outgoing::{
        tcp::{DaemonTcpOutgoing, LayerTcpOutgoing},
        LayerClose, LayerConnect, LayerWrite, SocketAddress,
    },
    ClientMessage, ConnectionId, DaemonMessage, LogLevel, CLIENT_READY_FOR_LOGS,
};
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{
        tcp::{OwnedReadHalf, OwnedWriteHalf},
        TcpListener,
    },
    select,
    sync::{
        mpsc::{self, Receiver, Sender},
        oneshot,
    },
};
use tokio_stream::{wrappers::TcpListenerStream, StreamExt, StreamMap};
use tokio_util::io::ReaderStream;

use crate::{connection::AgentConnection, CliError, PortMapping};

pub struct PortForwarder {
    // communicates with the agent (only TCP supported)
    agent_connection: AgentConnection,
    // associates local ports with destination ports
    mappings: HashMap<SocketAddr, SocketAddr>,
    // accepts connections from the user app in the form of a stream
    listeners: StreamMap<SocketAddr, TcpListenerStream>,
    // oneshot channels for sending connection IDs to tasks
    oneshots: VecDeque<oneshot::Sender<ConnectionId>>,
    // identifies tasks by their corresponding local socket address
    tasks: HashMap<SocketAddr, Sender<Vec<u8>>>,

    // transmit internal messages from tasks to main loop
    internal_msg_tx: Sender<PortForwardMessage>,
    internal_msg_rx: Receiver<PortForwardMessage>,

    // true if Ping has been sent to agent
    waiting_for_pong: bool,
    ping_pong_timeout: Instant,
}

enum PortForwardMessage {
    Connect(SocketAddr, oneshot::Sender<ConnectionId>),
    Send(ConnectionId, Vec<u8>),
    Close(ConnectionId),
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

        let (internal_msg_tx, internal_msg_rx) = mpsc::channel(16);

        Ok(Self {
            agent_connection,
            mappings,
            listeners,
            oneshots: VecDeque::new(),
            tasks: HashMap::new(),
            internal_msg_tx,
            internal_msg_rx,
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
                                        // TODO: transmit connection id
                                        },
                                    DaemonTcpOutgoing::Read(res) => todo!(), // TODO: send data to corresponding task
                                    DaemonTcpOutgoing::Close(connection_id) => {
                                        // TODO: remove from tasks hashmap, kill task
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
                        let task_internal_tx = self.internal_msg_tx.clone();
                        let Some(remote_socket) = self.mappings.get(&socket).cloned() else {
                            return Err(PortForwardError::SocketMappingNotFound(socket))
                        };
                        let (response_tx, mut response_rx) = mpsc::channel(16);
                        self.tasks.insert(socket, response_tx);
                        tokio::spawn(async move {
                            let (mut read, mut write) = stream.into_split();
                            let mut buffer = [];
                            let _ = read.read(&mut buffer).await;
                            let (oneshot_tx, oneshot_rx) = oneshot::channel::<ConnectionId>();
                            let _ = task_internal_tx.send(PortForwardMessage::Connect(remote_socket, oneshot_tx)).await;
                            let Ok(connection_id) = oneshot_rx.blocking_recv() else {
                                return Err(PortForwardError::InternalMessageError(format!("Error while trying to receive connection ID for socket address {socket}")));
                            };
                            let _ = task_internal_tx.send(PortForwardMessage::Send(connection_id, buffer.into())).await;
                            let mut read_stream = ReaderStream::new(read);

                            loop {
                                select! {
                                    message = read_stream.next() => match message {
                                        Some(Ok(message)) => {
                                            let _ = task_internal_tx.send(PortForwardMessage::Send(connection_id, message.into())).await;
                                        },
                                        Some(Err(error)) => {
                                            break Err(PortForwardError::TcpListenerError(error));
                                        },
                                        None => {
                                            let _ = task_internal_tx.send(PortForwardMessage::Close(connection_id)).await;
                                            break Ok(());
                                        },
                                    },

                                    message = response_rx.recv() => match message {
                                        Some(message) => {
                                            let _ = write.write(message.as_ref());
                                        },
                                        None => break Ok(()),
                                    }
                                }
                            }
                        });
                    },
                    Some((socket, Err(error))) => {
                        // error from TcpStream
                        tracing::error!("Error occured while listening to local socket {socket}: {error}");
                        // TODO: remove from listeners
                    },
                    None => break Ok(()),
                }
            }
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

    #[error("TcpListener operation failed with error: `{0}`")]
    TcpListenerError(std::io::Error),

    #[error("No destination address found for local address `{0}`")]
    SocketMappingNotFound(SocketAddr),

    #[error("Failed to establish connection with remote process: `{0}`")]
    ConnectionError(String),

    #[error("Failed to send or receive messages between PortForward and peripheral tasks with error: `{0}`")]
    InternalMessageError(String),
}

impl From<PortForwardError> for CliError {
    fn from(value: PortForwardError) -> Self {
        CliError::PortForwardingError(value)
    }
}
