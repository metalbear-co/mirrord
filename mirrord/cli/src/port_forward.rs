use std::{
    collections::{HashMap, VecDeque},
    net::SocketAddr,
    time::{Duration, Instant},
};

use mirrord_protocol::{
    outgoing::{
        tcp::{DaemonTcpOutgoing, LayerTcpOutgoing},
        LayerClose, LayerConnect, LayerWrite, SocketAddress,
    },
    ClientMessage, ConnectionId, DaemonMessage, LogLevel, CLIENT_READY_FOR_LOGS,
};
use thiserror::Error;
use tokio::{
    io::AsyncWriteExt,
    net::TcpListener,
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
    // identifies socket addresses by their corresponding connection ID
    sockets: HashMap<ConnectionId, SocketAddr>,
    // identifies tasks by their corresponding local socket address
    // for sending data from the remote socket to the local address
    tasks: HashMap<SocketAddr, Sender<Vec<u8>>>,

    // transmit internal messages from tasks to main loop
    internal_msg_tx: Sender<PortForwardMessage>,
    internal_msg_rx: Receiver<PortForwardMessage>,

    // true if Ping has been sent to agent
    waiting_for_pong: bool,
    ping_pong_timeout: Instant,
}

/// Used by tasks for individual forwarding connections to send instructions to main loop
enum PortForwardMessage {
    Connect(SocketAddr, oneshot::Sender<ConnectionId>),
    Send(ConnectionId, Vec<u8>),
    Close(SocketAddr, ConnectionId),
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
            sockets: HashMap::new(),
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
                                        match res {
                                            Ok(res) => {
                                                let SocketAddress::Ip(socket) = res.local_address else {
                                                    break Err(PortForwardError::ConnectionError
                                                        ("Unexpectedly received Unix address for socket during setup".into())
                                                    );
                                                };
                                                let Some(channel) = self.oneshots.pop_front() else {
                                                    break Err(PortForwardError::ReadyTaskNotFound(socket, res.connection_id));
                                                };
                                                self.sockets.insert(res.connection_id, socket);
                                                let _ = channel.send(res.connection_id);
                                            },
                                            Err(error) => break Err(PortForwardError::AgentError(format!("Problem receiving DaemonTcpOutgoing::Connect {error}"))),
                                        }
                                    },
                                    DaemonTcpOutgoing::Read(res) => {
                                        match res {
                                            Ok(res) => {
                                                let Some(&socket) = self.sockets.get(&res.connection_id) else {
                                                    break Err(PortForwardError::SocketFromIdNotFound(res.connection_id));
                                                };
                                                let Some(sender) = self.tasks.get(&socket) else {
                                                    break Err(PortForwardError::SenderNotFound(socket, res.connection_id));
                                                };
                                                let Ok(_) = sender.send(res.bytes).await else {
                                                    break Err(PortForwardError::InternalMessageError("Failed to send data from remote to task".into()))
                                                };
                                            },
                                            Err(error) => break Err(PortForwardError::AgentError(format!("Problem receiving DaemonTcpOutgoing::Read {error}"))),
                                        }
                                    },
                                    DaemonTcpOutgoing::Close(connection_id) => {
                                        let Some(socket) = self.sockets.remove(&connection_id) else {
                                            break Err(PortForwardError::SocketFromIdNotFound(connection_id));
                                        };
                                        let Some(sender) = self.tasks.remove(&socket) else {
                                            break Err(PortForwardError::SenderNotFound(socket, connection_id));
                                        };
                                        drop(sender);
                                        tracing::trace!("Connection closed for socket {socket}, connection {connection_id}");
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
                            let (read, mut write) = stream.into_split();
                            let mut read_stream = ReaderStream::new(read);
                            let (oneshot_tx, oneshot_rx) = oneshot::channel::<ConnectionId>();
                            let _ = task_internal_tx.send(PortForwardMessage::Connect(remote_socket, oneshot_tx)).await;
                            let Ok(connection_id) = oneshot_rx.await else {
                                return Err(PortForwardError::InternalMessageError(format!("Error while trying to receive connection ID for socket address {socket}")));
                            };

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
                                            let _ = task_internal_tx.send(PortForwardMessage::Close(socket, connection_id)).await;
                                            break Ok(());
                                        },
                                    },

                                    message = response_rx.recv() => match message {
                                        Some(message) => {
                                            match write.write(message.as_ref()).await {
                                                Ok(_) => continue,
                                                Err(error) => break Err(PortForwardError::InternalMessageError(format!("Failed to write data to local port in task: {error}"))),
                                            }
                                        },
                                        None => break Ok(()), // PortForwarder received DaemonTcpOutgoing::Close
                                    }
                                }
                            }
                        });
                    },
                    Some((socket, Err(error))) => {
                        // error from TcpStream
                        tracing::error!("Error occured while listening to local socket {socket}: {error}");
                        self.listeners.remove(&socket);
                    },
                    None => break Ok(()),
                },

                message = self.internal_msg_rx.recv() => match message {
                    Some(PortForwardMessage::Connect(remote_socket, oneshot)) => {
                        let remote_address = SocketAddress::Ip(remote_socket);
                        self.oneshots.push_back(oneshot);
                        let Ok(_) = self.agent_connection.sender.send(ClientMessage::TcpOutgoing(LayerTcpOutgoing::Connect(
                            LayerConnect { remote_address }
                        ))).await else {
                            break Err(PortForwardError::AgentError("Failed to send ClientMessage to agent".into()));
                        };
                    },
                    Some(PortForwardMessage::Send(connection_id, bytes)) => {
                        let Ok(_) = self.agent_connection.sender.send(ClientMessage::TcpOutgoing(LayerTcpOutgoing::Write(
                            LayerWrite { connection_id, bytes }
                        ))).await else {
                            break Err(PortForwardError::AgentError("Failed to send ClientMessage to agent".into()));
                        };
                    },
                    Some(PortForwardMessage::Close(socket, connection_id)) => {
                        // end of listener TCP stream in task
                        let Ok(_) = self.agent_connection.sender.send(ClientMessage::TcpOutgoing(LayerTcpOutgoing::Close(
                            LayerClose { connection_id }
                        ))).await else {
                            break Err(PortForwardError::AgentError("Failed to send ClientMessage to agent".into()));
                        };
                        self.listeners.remove(&socket);
                        self.tasks.remove(&socket);
                    },
                    None => {
                        tracing::trace!("PortForwardMessage Sender disconnected while sending");
                    },
                },
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

    #[error("No socket found for connection ID `{0}`")]
    SocketFromIdNotFound(ConnectionId),

    #[error("No task for socket {0} ready to receive connection ID: `{1}`")]
    ReadyTaskNotFound(SocketAddr, ConnectionId),

    #[error("No sender for socket {0} and connection ID: `{1}`")]
    SenderNotFound(SocketAddr, ConnectionId),

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

#[cfg(test)]
mod test {
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

    use crate::{connection::AgentConnection, port_forward::PortForwarder, PortMapping};

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
        let parsed_mappings = vec![PortMapping {
            local: local_destination,
            remote: "152.37.40.40:3038".parse().unwrap(),
        }];

        let mut port_forwarder = PortForwarder::new(agent_connection, parsed_mappings)
            .await
            .unwrap();
        let join_handle = tokio::spawn(async move { port_forwarder.run().await.unwrap() });

        // send data to socket
        let mut stream = TcpStream::connect(local_destination).await.unwrap();
        stream.write(b"data-my-beloved").await.unwrap();

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

        // expect Connect on client_msg_rx
        let remote_address = SocketAddress::Ip("152.37.40.40:3038".parse().unwrap());
        let local_address = SocketAddress::Ip(local_destination);
        let expected = ClientMessage::TcpOutgoing(LayerTcpOutgoing::Connect(LayerConnect {
            remote_address: remote_address.clone(),
        }));
        let message = match client_msg_rx.recv().await.ok_or(0).unwrap() {
            ClientMessage::Ping => client_msg_rx.recv().await.ok_or(0).unwrap(),
            other @ _ => other,
        };
        assert_eq!(message, expected);

        // reply with successful on daemon_msg_tx
        daemon_msg_tx
            .send(DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Connect(Ok(
                DaemonConnect {
                    connection_id: 1,
                    remote_address,
                    local_address,
                },
            ))))
            .await
            .unwrap();

        // expect data sent first to be received first
        stream.write(b"data-my-beloathed").await.unwrap();
        let expected = ClientMessage::TcpOutgoing(LayerTcpOutgoing::Write(LayerWrite {
            connection_id: 1,
            bytes: b"data-my-beloveddata-my-beloathed".to_vec(),
        }));
        let message = match client_msg_rx.recv().await.ok_or(0).unwrap() {
            ClientMessage::Ping => client_msg_rx.recv().await.ok_or(0).unwrap(),
            other @ _ => other,
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
        stream.read(&mut buf).await.unwrap();
        assert_eq!(buf, b"reply-my-beloved".as_ref());

        // ensure portforwarder closes when agent ends
        drop(daemon_msg_tx);
        join_handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_multiple_mappings_forwarding() {
        let remote_destination_1 = "152.37.40.40:1018".parse().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local_destination_1 = listener.local_addr().unwrap();
        drop(listener);

        let remote_destination_2 = "152.37.40.40:2028".parse().unwrap();
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
            PortMapping {
                local: local_destination_1,
                remote: remote_destination_1,
            },
            PortMapping {
                local: local_destination_2,
                remote: remote_destination_2,
            },
        ];

        let mut port_forwarder = PortForwarder::new(agent_connection, parsed_mappings)
            .await
            .unwrap();
        tokio::spawn(async move { port_forwarder.run().await.unwrap() });

        // send data to each socket
        let mut stream_1 = TcpStream::connect(local_destination_1).await.unwrap();
        let mut stream_2 = TcpStream::connect(local_destination_2).await.unwrap();

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

        // expect each Connect on client_msg_rx with correct mappings
        let remote_address_1 = SocketAddress::Ip(remote_destination_1);
        let local_address_1 = SocketAddress::Ip(local_destination_1);
        let expected = ClientMessage::TcpOutgoing(LayerTcpOutgoing::Connect(LayerConnect {
            remote_address: remote_address_1.clone(),
        }));
        let message = match client_msg_rx.recv().await.ok_or(0).unwrap() {
            ClientMessage::Ping => client_msg_rx.recv().await.ok_or(0).unwrap(),
            other @ _ => other,
        };
        assert_eq!(message, expected);

        let remote_address_2 = SocketAddress::Ip(remote_destination_2);
        let local_address_2 = SocketAddress::Ip(local_destination_2);
        let expected = ClientMessage::TcpOutgoing(LayerTcpOutgoing::Connect(LayerConnect {
            remote_address: remote_address_2.clone(),
        }));
        let message = match client_msg_rx.recv().await.ok_or(0).unwrap() {
            ClientMessage::Ping => client_msg_rx.recv().await.ok_or(0).unwrap(),
            other @ _ => other,
        };
        assert_eq!(message, expected);

        // reply with successful on each daemon_msg_tx
        daemon_msg_tx
            .send(DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Connect(Ok(
                DaemonConnect {
                    connection_id: 1,
                    remote_address: remote_address_1,
                    local_address: local_address_1,
                },
            ))))
            .await
            .unwrap();
        daemon_msg_tx
            .send(DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Connect(Ok(
                DaemonConnect {
                    connection_id: 2,
                    remote_address: remote_address_2,
                    local_address: local_address_2,
                },
            ))))
            .await
            .unwrap();

        // expect data to be received
        stream_1.write(b"data-from-1").await.unwrap();
        let expected = ClientMessage::TcpOutgoing(LayerTcpOutgoing::Write(LayerWrite {
            connection_id: 1,
            bytes: b"data-from-1".to_vec(),
        }));
        let message = match client_msg_rx.recv().await.ok_or(0).unwrap() {
            ClientMessage::Ping => client_msg_rx.recv().await.ok_or(0).unwrap(),
            other @ _ => other,
        };
        assert_eq!(message, expected);

        stream_2.write(b"data-from-2").await.unwrap();
        let expected = ClientMessage::TcpOutgoing(LayerTcpOutgoing::Write(LayerWrite {
            connection_id: 2,
            bytes: b"data-from-2".to_vec(),
        }));
        let message = match client_msg_rx.recv().await.ok_or(0).unwrap() {
            ClientMessage::Ping => client_msg_rx.recv().await.ok_or(0).unwrap(),
            other @ _ => other,
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
        stream_1.read(&mut buf).await.unwrap();
        assert_eq!(buf, b"reply-to-1".as_ref());
        let mut buf = [0; 10];
        stream_2.read(&mut buf).await.unwrap();
        assert_eq!(buf, b"reply-to-2".as_ref());
    }
}
