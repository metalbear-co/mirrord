use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    path::PathBuf,
};

use futures::StreamExt;
use mirrord_protocol::{
    tcp::{DaemonTcp, LayerTcpSteal, NewTcpConnection, TcpClose, TcpData},
    ConnectionId, Port,
};
use streammap_ext::StreamMap;
use tokio::{
    io::{AsyncWriteExt, ReadHalf, WriteHalf},
    net::{TcpListener, TcpStream},
    select,
    sync::mpsc::{Receiver, Sender},
};
use tokio_util::{io::ReaderStream, sync::CancellationToken};
use tracing::{debug, error, info, log::warn};

use self::ip_tables::SafeIpTables;
use crate::{
    error::{AgentError, Result},
    runtime::set_namespace,
    util::{ClientID, IndexAllocator, Subscriptions},
};

mod ip_tables;
mod orig_dst;

/// Commands from the agent that are passed down to the stealer worker, through [`TcpStealerAPI`].
///
/// These are the operations that the agent receives from the layer to make the _steal_ feature
/// work.
#[derive(Debug)]
enum Command {
    /// Passes the channel that's used by the worker to respond back to the (agent -> layer).
    NewAgent(Sender<DaemonTcp>),

    /// A layer wants to subscribe to this [`Port`].
    ///
    /// The agent starts stealing traffic on this [`Port`].
    Subscribe(Port),

    /// A layer wants to unsubscribe from this [`Port`].
    ///
    /// The agent stops stealing traffic from this [`Port`].
    UnsubscribePort(Port),

    /// Part of the [`Drop`] implementation of [`TcpStealerAPI`].
    ///
    /// Closes a layer connection, and unsubscribe its ports.
    AgentClosed,
}

/// Association between a client (identified by the `client_id`) and a [`Command`].
///
/// The (agent -> worker) channel uses this, instead of naked [`Command`]s when communicating.
#[derive(Debug)]
pub struct StealerCommand {
    /// Identifies which layer instance is sending the [`Command`].
    client_id: ClientID,

    /// The command message sent from (layer -> agent) to be handled by the steal worker.
    command: Command,
}

#[derive(Debug)]
pub(super) struct TcpStealerApi {
    /// Identifies which layer instance is associated with this API.
    client_id: ClientID,

    /// Channel that allows the agent to communicate with the stealer task.
    ///
    /// The agent controls the stealer task through this.
    command_tx: Sender<StealerCommand>,

    /// Channel that receives [`DaemonTcp`] messages from the stealer worker thread.
    ///
    /// This is where we get the messages that should be passed back to agent or layer.
    daemon_rx: Receiver<DaemonTcp>,
}

impl TcpStealerApi {
    #[tracing::instrument(level = "debug")]
    pub(super) async fn new(
        client_id: ClientID,
        command_tx: Sender<StealerCommand>,
        (daemon_tx, daemon_rx): (Sender<DaemonTcp>, Receiver<DaemonTcp>),
    ) -> Result<Self, AgentError> {
        command_tx
            .send(StealerCommand {
                client_id,
                command: Command::NewAgent(daemon_tx),
            })
            .await?;

        Ok(Self {
            client_id,
            command_tx,
            daemon_rx,
        })
    }
}

/// Created once per agent during initialization.
///
/// Runs as a separate thread while the agent lives.
///
/// - (agent -> stealer) communication is handled by [`command_rx`];
/// - (stealer -> agent) communication is handled by [`client_senders`], and the [`Sender`] channels
///   come inside [`StealerCommand`]s through  [`command_rx`];
#[derive(Debug)]
pub(super) struct TcpConnectionStealer {
    port_subscriptions: Subscriptions<Port, ClientID>,

    /// Communication between (agent -> stealer) task.
    ///
    /// The agent controls the stealer task through [`TcpStealerAPI::command_tx`].
    command_rx: Receiver<StealerCommand>,

    /// Connected clients (layer instances) and the channels which the stealer task uses to send
    /// back messages (stealer -> agent -> layer).
    client_senders: HashMap<ClientID, Sender<DaemonTcp>>,
    index_allocator: IndexAllocator<ConnectionId>,
}

impl TcpConnectionStealer {
    #[tracing::instrument(level = "debug")]
    pub(super) async fn new(
        command_rx: Receiver<StealerCommand>,
        pid: Option<u64>,
    ) -> Result<Self, AgentError> {
        if let Some(pid) = pid {
            let namespace = PathBuf::from("/proc")
                .join(PathBuf::from(pid.to_string()))
                .join(PathBuf::from("ns/net"));

            set_namespace(namespace).unwrap();
        }

        Ok(Self {
            port_subscriptions: Subscriptions::new(),
            command_rx,
            client_senders: HashMap::with_capacity(8),
            index_allocator: IndexAllocator::new(),
        })
    }

    // TODO(alex) [low] 2022-12-01: Better docs.
    /// Runs the stealer loop.
    #[tracing::instrument(level = "trace", skip(self))]
    pub(super) async fn start(
        mut self,
        cancellation_token: CancellationToken,
    ) -> Result<(), AgentError> {
        loop {
            select! {
                command = self.command_rx.recv() => {
                    if let Some(command) = command {
                        self.handle_command(command).await?;
                    } else { break; }
                },
                // TODO(alex) [mid] 2022-12-01: This should be global as well?
                //
                // Like steal everything or only steal what the users asked for?
                //
                // If we do this, then we break stuff, as this would mean we take every `TcpStream`.
                // accept = listener.accept() => todo!()
                _ = cancellation_token.cancelled() => {
                    break;
                }
            }
        }

        Ok(())
    }

    #[tracing::instrument(level = "debug", skip(self, sender))]
    fn new_client(&mut self, client_id: ClientID, sender: Sender<DaemonTcp>) {
        self.client_senders.insert(client_id, sender);
    }

    #[tracing::instrument(level = "debug", skip(self))]
    async fn subscribe(&mut self, client_id: ClientID, port: Port) -> Result<(), AgentError> {
        self.port_subscriptions.subscribe(client_id, port);
        // self.update_stealer()?;
        self.send_message_to_single_client(&client_id, DaemonTcp::Subscribed)
            .await
    }

    /// Removes the client with `client_id`, and also unsubscribes its port.
    #[tracing::instrument(level = "trace", skip(self))]
    fn close_client(&mut self, client_id: ClientID) -> Result<(), AgentError> {
        self.client_senders.remove(&client_id);
        self.port_subscriptions.remove_client(client_id);
        // self.update_sniffer()
        todo!()
    }

    /// Sends a [`DaemonTcp`] message back to the client with `client_id`.
    // #[tracing::instrument(level = "trace", skip(self))]
    async fn send_message_to_single_client(
        &mut self,
        client_id: &ClientID,
        message: DaemonTcp,
    ) -> Result<(), AgentError> {
        if let Some(sender) = self.client_senders.get(client_id) {
            sender.send(message).await.map_err(|err| {
                warn!(
                    "Failed to send message to client {} with {:#?}!",
                    client_id, err
                );
                let _ = self.close_client(*client_id);
                err
            })?;
        }

        Ok(())
    }

    // #[tracing::instrument(level = "trace", skip(self, clients))]
    async fn send_message_to_all_clients(
        &mut self,
        clients: impl Iterator<Item = &ClientID>,
        message: DaemonTcp,
    ) -> Result<(), AgentError> {
        for client_id in clients {
            self.send_message_to_single_client(client_id, message.clone())
                .await?;
        }
        Ok(())
    }

    // #[tracing::instrument(level = "trace", skip(self))]
    async fn handle_command(&mut self, command: StealerCommand) -> Result<(), AgentError> {
        let StealerCommand { client_id, command } = command;

        match command {
            Command::NewAgent(daemon_tx) => todo!(),
            Command::Subscribe(port) => todo!(),
            Command::UnsubscribePort(port) => todo!(),
            Command::AgentClosed => todo!(),
        }

        Ok(())
    }
}

pub struct StealWorker {
    pub sender: Sender<DaemonTcp>,
    iptables: SafeIpTables<iptables::IPTables>,
    ports: HashSet<Port>,
    listen_port: Port,
    write_streams: HashMap<ConnectionId, WriteHalf<TcpStream>>,
    read_streams: StreamMap<ConnectionId, ReaderStream<ReadHalf<TcpStream>>>,
    connection_index: u64,
}

impl StealWorker {
    #[tracing::instrument(level = "trace", skip(sender))]
    pub fn new(sender: Sender<DaemonTcp>, listen_port: Port) -> Result<Self> {
        Ok(Self {
            sender,
            iptables: SafeIpTables::new(iptables::new(false).unwrap())?,
            ports: HashSet::default(),
            listen_port,
            write_streams: HashMap::default(),
            read_streams: StreamMap::default(),
            connection_index: 0,
        })
    }

    #[tracing::instrument(level = "trace", skip(self, rx, listener))]
    pub async fn start(
        mut self,
        mut rx: Receiver<LayerTcpSteal>,
        listener: TcpListener,
    ) -> Result<()> {
        loop {
            select! {
                msg = rx.recv() => {
                    if let Some(msg) = msg {
                        self.handle_client_message(msg).await?;
                    } else {
                        debug!("rx closed, breaking");
                        break;
                    }
                },
                accept = listener.accept() => {
                    match accept {
                        Ok((stream, address)) => {
                            self.handle_incoming_connection(stream, address).await?;
                        },
                        Err(err) => {
                            error!("accept error {err:?}");
                            break;
                        }
                    }
                },
                message = self.next() => {
                    if let Some(message) = message {
                        self.sender.send(message).await?;
                    }
                }
            }
        }
        debug!("TCP Stealer exiting");
        Ok(())
    }

    #[tracing::instrument(level = "trace", skip(self))]
    pub async fn handle_client_message(&mut self, message: LayerTcpSteal) -> Result<()> {
        use LayerTcpSteal::*;
        match message {
            PortSubscribe(port) => {
                if self.ports.contains(&port) {
                    warn!("Port {port:?} is already subscribed");
                    Ok(())
                } else {
                    debug!("adding redirect rule");
                    self.iptables.add_redirect(port, self.listen_port)?;
                    self.ports.insert(port);
                    self.sender.send(DaemonTcp::Subscribed).await?;
                    debug!("sent subscribed");
                    Ok(())
                }
            }
            ConnectionUnsubscribe(connection_id) => {
                info!("Closing connection {connection_id:?}");
                self.write_streams.remove(&connection_id);
                self.read_streams.remove(&connection_id);
                Ok(())
            }
            PortUnsubscribe(port) => {
                if self.ports.remove(&port) {
                    self.iptables.remove_redirect(port, self.listen_port)
                } else {
                    warn!("removing unsubscribed port {port:?}");
                    Ok(())
                }
            }

            Data(data) => {
                if let Some(stream) = self.write_streams.get_mut(&data.connection_id) {
                    stream.write_all(&data.bytes[..]).await?;
                    Ok(())
                } else {
                    warn!(
                        "Trying to send data to closed connection {:?}",
                        data.connection_id
                    );
                    Ok(())
                }
            }
        }
    }

    #[tracing::instrument(level = "trace", skip(self, stream))]
    pub async fn handle_incoming_connection(
        &mut self,
        stream: TcpStream,
        address: SocketAddr,
    ) -> Result<()> {
        let real_addr = orig_dst::orig_dst_addr(&stream)?;
        if !self.ports.contains(&real_addr.port()) {
            return Err(AgentError::UnexpectedConnection(real_addr.port()));
        }
        let connection_id = self.connection_index;
        self.connection_index += 1;

        let (read_half, write_half) = tokio::io::split(stream);
        self.write_streams.insert(connection_id, write_half);
        self.read_streams
            .insert(connection_id, ReaderStream::new(read_half));

        let new_connection = DaemonTcp::NewConnection(NewTcpConnection {
            connection_id,
            destination_port: real_addr.port(),
            source_port: address.port(),
            address: address.ip(),
        });
        self.sender.send(new_connection).await?;
        debug!("sent new connection");
        Ok(())
    }

    pub async fn next(&mut self) -> Option<DaemonTcp> {
        let (connection_id, value) = self.read_streams.next().await?;
        match value {
            Some(Ok(bytes)) => Some(DaemonTcp::Data(TcpData {
                connection_id,
                bytes: bytes.to_vec(),
            })),
            Some(Err(err)) => {
                error!("connection id {connection_id:?} read error: {err:?}");
                None
            }
            None => Some(DaemonTcp::Close(TcpClose { connection_id })),
        }
    }
}

#[tracing::instrument(level = "trace", skip(rx, tx))]
pub async fn steal_worker(
    rx: Receiver<LayerTcpSteal>,
    tx: Sender<DaemonTcp>,
    pid: Option<u64>,
) -> Result<()> {
    if let Some(pid) = pid {
        let namespace = PathBuf::from("/proc")
            .join(PathBuf::from(pid.to_string()))
            .join(PathBuf::from("ns/net"));

        set_namespace(namespace)?;
    }
    let listener = TcpListener::bind("0.0.0.0:0").await?;
    let listen_port = listener.local_addr()?.port();

    StealWorker::new(tx, listen_port)?.start(rx, listener).await
}
