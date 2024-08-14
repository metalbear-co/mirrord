use std::{
    collections::{HashMap, VecDeque},
    net::SocketAddr,
    time::{Duration, Instant},
};

use futures::StreamExt;
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

use crate::{connection::AgentConnection, PortMapping};

pub struct PortForwarder {
    /// communicates with the agent (only TCP supported)
    agent_connection: AgentConnection,
    /// associates local ports with destination ports
    mappings: HashMap<SocketAddr, SocketAddr>,
    /// accepts connections from the user app in the form of a stream
    listeners: StreamMap<SocketAddr, TcpListenerStream>,
    /// oneshot channels for sending connection IDs to tasks
    oneshots: VecDeque<oneshot::Sender<ConnectionId>>,
    /// identifies remote socket addresses by their corresponding connection ID
    sockets: HashMap<ConnectionId, SocketAddr>,
    /// identifies tasks by their corresponding remote socket address
    /// for sending data from the remote socket to the local address
    tasks: HashMap<SocketAddr, Sender<Vec<u8>>>,

    /// transmit internal messages from tasks to [`PortForwarder`]'s main loop.
    internal_msg_tx: Sender<PortForwardMessage>,
    internal_msg_rx: Receiver<PortForwardMessage>,

    /// true if Ping has been sent to agent
    waiting_for_pong: bool,
    ping_pong_timeout: Instant,
}

/// Used by tasks for individual forwarding connections to send instructions to [`PortForwarder`]'s
/// main loop.
#[derive(Debug)]
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
        tracing::trace!("port forwarding setup complete");

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
                        let Some(channel) = self.oneshots.pop_front() else {
                            return Err(PortForwardError::ReadyTaskNotFound(
                                remote_socket,
                                connection_id,
                            ));
                        };
                        self.sockets.insert(connection_id, remote_socket);
                        let Ok(_) = channel.send(connection_id) else {
                            self.agent_connection
                                .sender
                                .send(ClientMessage::TcpOutgoing(LayerTcpOutgoing::Close(
                                    LayerClose { connection_id },
                                )))
                                .await?;
                            return Err(PortForwardError::InternalMessageError(format!(
                                "failed to send connection ID {connection_id} to task on oneshot channel"
                            )));
                        };
                        tracing::trace!("successful connection to remote address {remote_socket}, connection ID is {}", connection_id);
                    }
                    Err(error) => {
                        tracing::error!("failed to connect to a remote address: {error}");
                        let _ = self.oneshots.pop_front();
                    }
                },
                DaemonTcpOutgoing::Read(res) => match res {
                    Ok(res) => {
                        let Some(&remote_socket) = self.sockets.get(&res.connection_id) else {
                            // ignore unknown connection IDs
                            return Ok(());
                        };
                        let Some(sender) = self.tasks.get(&remote_socket) else {
                            unreachable!("sender is always created before this point")
                        };
                        match sender.send(res.bytes).await {
                            Ok(_) => (),
                            Err(_) => {
                                self.tasks.remove(&remote_socket);
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
                    let Some(remote_socket) = self.sockets.remove(&connection_id) else {
                        // ignore unknown connection IDs
                        return Ok(());
                    };
                    let Some(_sender) = self.tasks.remove(&remote_socket) else {
                        unreachable!("sender is always created before this point")
                    };
                    tracing::trace!(
                        "connection closed for remote socket {remote_socket}, connection {connection_id}"
                    );
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
        let Some(remote_socket) = self.mappings.get(&local_socket).cloned() else {
            unreachable!("mappings are always created before this point")
        };
        let (response_tx, response_rx) = mpsc::channel(256);
        self.tasks.insert(remote_socket, response_tx);

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
            PortForwardMessage::Connect(remote_socket, oneshot) => {
                let remote_address = SocketAddress::Ip(remote_socket);
                self.oneshots.push_back(oneshot);
                let Ok(_) = self
                    .agent_connection
                    .sender
                    .send(ClientMessage::TcpOutgoing(LayerTcpOutgoing::Connect(
                        LayerConnect { remote_address },
                    )))
                    .await
                else {
                    return Err(PortForwardError::AgentError(
                        "failed to send ClientMessage to agent".into(),
                    ));
                };
            }
            PortForwardMessage::Send(connection_id, bytes) => {
                let Ok(_) = self
                    .agent_connection
                    .sender
                    .send(ClientMessage::TcpOutgoing(LayerTcpOutgoing::Write(
                        LayerWrite {
                            connection_id,
                            bytes,
                        },
                    )))
                    .await
                else {
                    return Err(PortForwardError::AgentError(
                        "failed to send ClientMessage to agent".into(),
                    ));
                };
            }
            PortForwardMessage::Close(socket, connection_id) => {
                // end of listener TCP stream in task
                self.agent_connection
                    .sender
                    .send(ClientMessage::TcpOutgoing(LayerTcpOutgoing::Close(
                        LayerClose { connection_id },
                    )))
                    .await?;
                self.tasks.remove(&socket);
            }
        }
        Ok(())
    }
}

struct LocalConnectionTask {
    read_stream: ReaderStream<OwnedReadHalf>,
    write: OwnedWriteHalf,
    local_socket: SocketAddr,
    remote_socket: SocketAddr,
    task_internal_tx: Sender<PortForwardMessage>,
    response_rx: Receiver<Vec<u8>>,
}

impl LocalConnectionTask {
    pub fn new(
        stream: TcpStream,
        local_socket: SocketAddr,
        remote_socket: SocketAddr,
        task_internal_tx: Sender<PortForwardMessage>,
        response_rx: Receiver<Vec<u8>>,
    ) -> Self {
        let (read, write) = stream.into_split();
        let read_stream = ReaderStream::with_capacity(read, 64000);
        Self {
            remote_socket,
            task_internal_tx,
            response_rx,
            read_stream,
            write,
            local_socket,
        }
    }

    pub async fn run(&mut self) -> Result<(), PortForwardError> {
        let (oneshot_tx, oneshot_rx) = oneshot::channel::<ConnectionId>();

        // lazy connection: wait until data starts
        let first = match self.read_stream.next().await {
            Some(Ok(data)) => data,
            Some(Err(error)) => return Err(PortForwardError::TcpListenerError(error)),
            None => {
                // stream ended without sending data
                return Ok(());
            }
        };

        match self
            .task_internal_tx
            .send(PortForwardMessage::Connect(self.remote_socket, oneshot_tx))
            .await
        {
            Ok(_) => (),
            Err(error) => {
                return Err(PortForwardError::InternalMessageError(format!(
                    "error while trying to send connect request to main loop: {error}"
                )));
            }
        };
        let Ok(connection_id) = oneshot_rx.await else {
            return Err(PortForwardError::InternalMessageError(format!(
                "error while trying to receive connection ID for socket address {}",
                self.local_socket
            )));
        };
        match self
            .task_internal_tx
            .send(PortForwardMessage::Send(connection_id, first.into()))
            .await
        {
            Ok(_) => (),
            Err(error) => {
                return Err(PortForwardError::InternalMessageError(format!(
                    "failed to send data to main loop: {error}"
                )))
            }
        };

        loop {
            select! {
                message = self.read_stream.next() => match message {
                    Some(Ok(message)) => {
                        match self.task_internal_tx
                            .send(PortForwardMessage::Send(connection_id, message.into()))
                            .await
                        {
                            Ok(_) => (),
                            Err(error) => {
                                break Err(PortForwardError::InternalMessageError(format!(
                                    "failed to send data to main loop: {error}"
                                )))
                            }
                        };
                    },
                    Some(Err(error)) => {
                        tracing::error!("tcp listener error: {error}");
                        break Ok(());
                    },
                    None => {
                        let _ = self.task_internal_tx.send(PortForwardMessage::Close(self.remote_socket, connection_id)).await;
                        break Ok(());
                    },
                },

                message = self.response_rx.recv() => match message {
                    Some(message) => {
                        match self.write.write_all(message.as_ref()).await {
                            Ok(_) => continue,
                            Err(error) => {
                                tracing::error!("failed to write remote response to local destination: {error}");
                                break Ok(());
                            },
                        }
                    },
                    None => break Ok(()), // PortForwarder received DaemonTcpOutgoing::Close
                }
            }
        }
    }
}

#[derive(Debug, Error)]
pub enum PortForwardError {
    // setup errors
    #[error("multiple port forwarding mappings found for local address `{0}`")]
    PortMapSetupError(SocketAddr),

    #[error("no port forwarding mappings were provided")]
    NoMappingsError(),

    // running errors
    #[error("agent closed connection with error: `{0}`")]
    AgentError(String),

    #[error("connection with the agent failed")]
    AgentConnectionFailed,

    #[error("failed to send Ping to agent: `{0}`")]
    PingError(String),

    #[error("TcpListener operation failed with error: `{0}`")]
    TcpListenerError(std::io::Error),

    #[error("no destination address found for local address `{0}`")]
    SocketMappingNotFound(SocketAddr),

    #[error("no task for socket {0} ready to receive connection ID: `{1}`")]
    ReadyTaskNotFound(SocketAddr, ConnectionId),

    #[error("failed to establish connection with remote process: `{0}`")]
    ConnectionError(String),

    #[error("failed to send or receive messages between PortForward and peripheral tasks with error: `{0}`")]
    InternalMessageError(String),
}

impl From<mpsc::error::SendError<ClientMessage>> for PortForwardError {
    fn from(_: mpsc::error::SendError<ClientMessage>) -> Self {
        Self::AgentConnectionFailed
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
        tokio::spawn(async move { port_forwarder.run().await.unwrap() });

        // send data to socket
        let mut stream = TcpStream::connect(local_destination).await.unwrap();
        stream.write_all(b"data-my-beloved").await.unwrap();

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

        // send data to first socket
        let mut stream_1 = TcpStream::connect(local_destination_1).await.unwrap();

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

        // expect each Connect on client_msg_rx with correct mappings when data has been written
        // (lazy)
        stream_1.write_all(b"data-from-1").await.unwrap();
        let remote_address_1 = SocketAddress::Ip(remote_destination_1);
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
        let remote_address_2 = SocketAddress::Ip(remote_destination_2);
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
