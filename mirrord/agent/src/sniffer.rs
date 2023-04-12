use std::{
    collections::{HashMap, HashSet},
    hash::{Hash, Hasher},
    net::{IpAddr, Ipv4Addr, SocketAddr},
};

use mirrord_protocol::{
    tcp::{DaemonTcp, LayerTcp, NewTcpConnection, TcpClose, TcpData},
    ConnectionId, Port,
};
use nix::sys::socket::SockaddrStorage;
use pnet::packet::{
    ethernet::{EtherTypes, EthernetPacket},
    ip::IpNextHeaderProtocols,
    ipv4::Ipv4Packet,
    tcp::{TcpFlags, TcpPacket},
    Packet,
};
use rawsocket::RawCapture;
use tokio::{
    net::UdpSocket,
    select,
    sync::mpsc::{self, Receiver, Sender},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, trace, warn};

use crate::{
    error::AgentError,
    util::{ClientId, IndexAllocator, Subscriptions},
    watched_task::TaskStatus,
};

#[derive(Debug, Eq, Copy, Clone)]
pub struct TcpSessionIdentifier {
    source_addr: Ipv4Addr,
    dest_addr: Ipv4Addr,
    source_port: u16,
    dest_port: u16,
}

impl PartialEq for TcpSessionIdentifier {
    /// It's the same session if 4 tuple is same/opposite.
    fn eq(&self, other: &TcpSessionIdentifier) -> bool {
        self.source_addr == other.source_addr
            && self.dest_addr == other.dest_addr
            && self.source_port == other.source_port
            && self.dest_port == other.dest_port
            || self.source_addr == other.dest_addr
                && self.dest_addr == other.source_addr
                && self.source_port == other.dest_port
                && self.dest_port == other.source_port
    }
}

impl Hash for TcpSessionIdentifier {
    fn hash<H: Hasher>(&self, state: &mut H) {
        if self.source_addr > self.dest_addr {
            self.source_addr.hash(state);
            self.dest_addr.hash(state);
        } else {
            self.dest_addr.hash(state);
            self.source_addr.hash(state);
        }
        if self.source_port > self.dest_port {
            self.source_port.hash(state);
            self.dest_port.hash(state);
        } else {
            self.dest_port.hash(state);
            self.source_port.hash(state);
        }
    }
}

#[derive(Debug)]
struct TCPSession {
    id: ConnectionId,
    clients: HashSet<ClientId>,
}

type TCPSessionMap = HashMap<TcpSessionIdentifier, TCPSession>;

fn is_new_connection(flags: u16) -> bool {
    0 != (flags & TcpFlags::SYN) && 0 == (flags & (TcpFlags::ACK | TcpFlags::RST | TcpFlags::FIN))
}

fn is_closed_connection(flags: u16) -> bool {
    0 != (flags & (TcpFlags::FIN | TcpFlags::RST))
}

/// Connects to a remote address (`8.8.8.8:53`) so we can find which network interface to use.
///
/// Used when no `user_interface` is specified in [`prepare_sniffer`] to prevent mirrord from
/// defaulting to the wrong network interface (`eth0`), as sometimes the user's machine doesn't have
/// it available (i.e. their default network is `enp2s0`).
#[tracing::instrument(level = "trace")]
async fn resolve_interface() -> Result<Option<String>, AgentError> {
    // Connect to a remote address so we can later get the default network interface.
    let temporary_socket = UdpSocket::bind("0.0.0.0:0").await?;
    temporary_socket.connect("8.8.8.8:53").await?;

    // Create comparison address here with `port: 0`, to match the network interface's address of
    // `sin_port: 0`.
    let local_address = SocketAddr::new(temporary_socket.local_addr()?.ip(), 0);
    let raw_local_address = SockaddrStorage::from(local_address);

    // Try to find an interface that matches the local ip we have.
    let usable_interface_name = nix::ifaddrs::getifaddrs()?
        .find_map(|iface| (raw_local_address == iface.address?).then_some(iface.interface_name));

    Ok(usable_interface_name)
}

// TODO(alex): Errors here are not reported back anywhere, we end up with a generic fail of:
// "ERROR ThreadId(03) mirrord_agent: ClientConnectionHandler::start -> Client 0 disconnected with
// error: SnifferCommand sender failed with `channel closed`"
//
// And to make matters worse, the error reported back to the user is the very generic:
// "mirrord-layer received an unexpected response from the agent pod!"
#[tracing::instrument(level = "trace")]
async fn prepare_sniffer(network_interface: Option<String>) -> Result<RawCapture, AgentError> {
    // Priority is whatever the user set as an option to mirrord, otherwise we try to get the
    // appropriate interface.
    let interface = if let Some(network_interface) = network_interface {
        network_interface
    } else {
        resolve_interface()
            .await?
            .unwrap_or_else(|| "eth0".to_string())
    };

    trace!("Using {interface:#?} interface.");
    let capture = RawCapture::from_interface_name(&interface)?;
    // We start with a BPF that drops everything so we won't receive *EVERYTHING*
    // as we don't know what the layer will ask us to listen for, so this is essentially setting
    // it to none
    // we ofc could've done this when a layer connected, but I (A.H) thought it'd make more sense
    // to have this shared among layers (potentially, in the future) - fme.
    capture.set_filter(rawsocket::filter::build_drop_always())?;
    capture
        .ignore_outgoing()
        .map_err(AgentError::PacketIgnoreOutgoing)?;
    Ok(capture)
}

#[derive(Debug)]
struct TcpPacketData {
    bytes: Vec<u8>,
    flags: u16,
}

#[tracing::instrument(skip(eth_packet), level = "trace", fields(bytes = %eth_packet.len()))]
fn get_tcp_packet(eth_packet: Vec<u8>) -> Option<(TcpSessionIdentifier, TcpPacketData)> {
    let eth_packet = EthernetPacket::new(&eth_packet[..])?;
    let ip_packet = match eth_packet.get_ethertype() {
        EtherTypes::Ipv4 => Ipv4Packet::new(eth_packet.payload())?,
        _ => return None,
    };

    let tcp_packet = match ip_packet.get_next_level_protocol() {
        IpNextHeaderProtocols::Tcp => TcpPacket::new(ip_packet.payload())?,
        _ => return None,
    };

    let dest_port = tcp_packet.get_destination();
    let source_port = tcp_packet.get_source();

    let identifier = TcpSessionIdentifier {
        source_addr: ip_packet.get_source(),
        dest_addr: ip_packet.get_destination(),
        source_port,
        dest_port,
    };

    trace!("identifier {identifier:?}");
    Some((
        identifier,
        TcpPacketData {
            flags: tcp_packet.get_flags(),
            bytes: tcp_packet.payload().to_vec(),
        },
    ))
}

#[derive(Debug)]
enum SnifferCommands {
    NewAgent(Sender<DaemonTcp>),
    Subscribe(Port),
    UnsubscribePort(Port),
    UnsubscribeConnection(ConnectionId),
    AgentClosed,
}

impl From<LayerTcp> for SnifferCommands {
    fn from(value: LayerTcp) -> Self {
        match value {
            LayerTcp::PortSubscribe(port) => Self::Subscribe(port),
            LayerTcp::PortUnsubscribe(port) => Self::UnsubscribePort(port),
            LayerTcp::ConnectionUnsubscribe(id) => Self::UnsubscribeConnection(id),
        }
    }
}

#[derive(Debug)]
pub struct SnifferCommand {
    client_id: ClientId,
    command: SnifferCommands,
}

/// Interface used by clients to interact with the [`TcpConnectionSniffer`].
/// Multiple instances of this struct operate on a single sniffer instance.
pub struct TcpSnifferApi {
    /// Id of the client using this struct.
    client_id: ClientId,
    /// Channel used to send commands to the [`TcpConnectionSniffer`].
    sender: Sender<SnifferCommand>,
    /// Channel used to receive messages from the [`TcpConnectionSniffer`].
    receiver: Receiver<DaemonTcp>,
    /// View on the sniffer task's status.
    task_status: TaskStatus,
}

impl TcpSnifferApi {
    /// Create a new instance of this struct and connect it to a [`TcpConnectionSniffer`] instance.
    /// * `client_id` - id of the client using this struct
    /// * `sniffer_sender` - channel used to send commands to the [`TcpConnectionSniffer`]
    /// * `task_status` - handle to the [`TcpConnectionSniffer`] exit status
    /// * `channel_size` - capacity of the channel connecting [`TcpConnectionSniffer`] back to this struct
    pub async fn new(
        client_id: ClientId,
        sniffer_sender: Sender<SnifferCommand>,
        task_status: TaskStatus,
        channel_size: usize,
    ) -> Result<TcpSnifferApi, AgentError> {
        let (sender, receiver) = mpsc::channel(channel_size);

        sniffer_sender
            .send(SnifferCommand {
                client_id,
                command: SnifferCommands::NewAgent(sender),
            })
            .await?;

        Ok(Self {
            client_id,
            sender: sniffer_sender,
            receiver,
            task_status,
        })
    }

    /// Send the given command to the connected [`TcpConnectionSniffer`].
    async fn send_command(&mut self, command: SnifferCommands) -> Result<(), AgentError> {
        let command = SnifferCommand {
            client_id: self.client_id,
            command,
        };

        if self.sender.send(command).await.is_ok() {
            Ok(())
        } else {
            Err(self.task_status.unwrap_err().await)
        }
    }

    /// Return the next message from the connected [`TcpConnectionSniffer`].
    pub async fn recv(&mut self) -> Result<DaemonTcp, AgentError> {
        match self.receiver.recv().await {
            Some(msg) => Ok(msg),
            None => Err(self.task_status.unwrap_err().await),
        }
    }

    /// Tansform the given message into a [`SnifferCommands`] and pass it to the connected [`TcpConnectionSniffer`].
    pub async fn handle_client_message(&mut self, message: LayerTcp) -> Result<(), AgentError> {
        self.send_command(message.into()).await
    }
}

impl Drop for TcpSnifferApi {
    fn drop(&mut self) {
        self.sender
            .try_send(SnifferCommand {
                client_id: self.client_id,
                command: SnifferCommands::AgentClosed,
            })
            .unwrap();
    }
}

pub struct TcpConnectionSniffer {
    port_subscriptions: Subscriptions<Port, ClientId>,
    receiver: Receiver<SnifferCommand>,
    client_senders: HashMap<ClientId, Sender<DaemonTcp>>,
    raw_capture: RawCapture,
    sessions: TCPSessionMap,
    //todo: impl drop for index allocator and connection id..
    connection_id_to_tcp_identifier: HashMap<ConnectionId, TcpSessionIdentifier>,
    index_allocator: IndexAllocator<ConnectionId>,
}

impl TcpConnectionSniffer {
    pub const TASK_NAME: &'static str = "Sniffer";

    /// Runs the sniffer loop, capturing packets.
    #[tracing::instrument(level = "trace", skip(self))]
    pub async fn start(mut self, cancel_token: CancellationToken) -> Result<(), AgentError> {
        loop {
            select! {
                command = self.receiver.recv() => {
                    if let Some(command) = command {
                        self.handle_command(command).await?;
                    } else { break; }
                },
                packet = self.raw_capture.next() => {
                    self.handle_packet(packet?).await?;
                }
                _ = cancel_token.cancelled() => {
                    break;
                }
            }
        }
        debug!("TCPConnectionSniffer exiting");
        Ok(())
    }

    /// Creates and prepares a new [`TcpConnectionSniffer`] that uses BPF filters to capture network
    /// packets.
    ///
    /// The capture uses a network interface specified by the user, if there is none, then it tries
    /// to find a proper one by starting a connection. If this fails, we use "eth0" as a last
    /// resort.
    #[tracing::instrument(level = "trace")]
    pub async fn new(
        receiver: Receiver<SnifferCommand>,
        network_interface: Option<String>,
    ) -> Result<Self, AgentError> {
        let raw_capture = prepare_sniffer(network_interface).await?;

        Ok(Self {
            receiver,
            raw_capture,
            port_subscriptions: Subscriptions::new(),
            client_senders: HashMap::new(),
            sessions: TCPSessionMap::new(),
            //todo: impl drop for index allocator and connection id..
            connection_id_to_tcp_identifier: HashMap::new(),
            index_allocator: Default::default(),
        })
    }

    #[tracing::instrument(level = "trace", skip(self, sender))]
    fn handle_new_client(&mut self, client_id: ClientId, sender: Sender<DaemonTcp>) {
        self.client_senders.insert(client_id, sender);
    }

    async fn handle_subscribe(
        &mut self,
        client_id: ClientId,
        port: Port,
    ) -> Result<(), AgentError> {
        self.port_subscriptions.subscribe(client_id, port);
        self.update_sniffer()?;
        self.send_message_to_client(&client_id, DaemonTcp::SubscribeResult(Ok(port)))
            .await
    }

    /// Removes the client with `client_id`, and also unsubscribes its port.
    #[tracing::instrument(level = "trace", skip(self))]
    fn handle_client_closed(&mut self, client_id: ClientId) -> Result<(), AgentError> {
        self.client_senders.remove(&client_id);
        self.port_subscriptions.remove_client(client_id);
        self.update_sniffer()
    }

    #[tracing::instrument(level = "trace", skip(self))]
    fn update_sniffer(&mut self) -> Result<(), AgentError> {
        let ports = self.port_subscriptions.get_subscribed_topics();

        if ports.is_empty() {
            trace!("Empty ports, setting dummy bpf");
            self.raw_capture
                .set_filter(rawsocket::filter::build_drop_always())?
        } else {
            self.raw_capture
                .set_filter(rawsocket::filter::build_tcp_port_filter(&ports))?
        };
        Ok(())
    }

    fn qualified_port(&self, port: u16) -> bool {
        self.port_subscriptions
            .get_subscribed_topics()
            .contains(&port)
    }

    async fn handle_command(&mut self, command: SnifferCommand) -> Result<(), AgentError> {
        match command {
            SnifferCommand {
                client_id,
                command: SnifferCommands::NewAgent(sender),
            } => {
                self.handle_new_client(client_id, sender);
            }
            SnifferCommand {
                client_id,
                command: SnifferCommands::Subscribe(port),
            } => {
                self.handle_subscribe(client_id, port).await?;
            }
            SnifferCommand {
                client_id,
                command: SnifferCommands::AgentClosed,
            } => {
                self.handle_client_closed(client_id)?;
            }
            SnifferCommand {
                client_id,
                command: SnifferCommands::UnsubscribeConnection(connection_id),
            } => {
                self.connection_id_to_tcp_identifier
                    .get(&connection_id)
                    .and_then(|identifier| {
                        self.sessions
                            .get_mut(identifier)
                            .map(|session| session.clients.remove(&client_id))
                    });
            }
            SnifferCommand {
                client_id,
                command: SnifferCommands::UnsubscribePort(port),
            } => {
                self.port_subscriptions.unsubscribe(client_id, port);
                self.update_sniffer()?;
            }
        }
        Ok(())
    }

    async fn send_message_to_clients(
        &mut self,
        clients: impl Iterator<Item = &ClientId>,
        message: DaemonTcp,
    ) -> Result<(), AgentError> {
        trace!("TcpConnectionSniffer::send_message_to_clients");

        for client_id in clients {
            self.send_message_to_client(client_id, message.clone())
                .await?;
        }
        Ok(())
    }

    /// Sends a [`DaemonTcp`] message back to the client with `client_id`.
    #[tracing::instrument(level = "trace", skip(self, message))]
    async fn send_message_to_client(
        &mut self,
        client_id: &ClientId,
        message: DaemonTcp,
    ) -> Result<(), AgentError> {
        if let Some(sender) = self.client_senders.get(client_id) {
            sender.send(message).await.map_err(|err| {
                warn!(
                    "Failed to send message to client {} with {:#?}!",
                    client_id, err
                );
                let _ = self.handle_client_closed(*client_id);
                err
            })?;
        }
        Ok(())
    }

    #[tracing::instrument(level = "trace", skip(self, eth_packet), fields(bytes = %eth_packet.len()))]
    async fn handle_packet(&mut self, eth_packet: Vec<u8>) -> Result<(), AgentError> {
        let (identifier, tcp_packet) = match get_tcp_packet(eth_packet) {
            Some(res) => res,
            None => return Ok(()),
        };

        let dest_port = identifier.dest_port;
        let source_port = identifier.source_port;
        let tcp_flags = tcp_packet.flags;
        trace!(
            "dest_port {:#?} | source_port {:#?} | tcp_flags {:#?}",
            dest_port,
            source_port,
            tcp_flags
        );

        let is_client_packet = self.qualified_port(dest_port);

        let session = match self.sessions.remove(&identifier) {
            Some(session) => session,
            None => {
                if !is_new_connection(tcp_flags) {
                    debug!("not new connection {tcp_flags:?}");
                    return Ok(());
                }

                if !is_client_packet {
                    return Ok(());
                }

                let id = match self.index_allocator.next_index() {
                    Some(id) => id,
                    None => {
                        error!("connection index exhausted, dropping new connection");
                        return Ok(());
                    }
                };

                let client_ids = self.port_subscriptions.get_topic_subscribers(dest_port);
                trace!("client_ids {:#?}", client_ids);

                let message = DaemonTcp::NewConnection(NewTcpConnection {
                    destination_port: dest_port,
                    source_port,
                    connection_id: id,
                    remote_address: IpAddr::V4(identifier.source_addr),
                    local_address: IpAddr::V4(identifier.dest_addr),
                });
                trace!("message {:#?}", message);

                self.send_message_to_clients(client_ids.iter(), message)
                    .await?;

                self.connection_id_to_tcp_identifier.insert(id, identifier);

                TCPSession {
                    id,
                    clients: client_ids.into_iter().collect(),
                }
            }
        };
        trace!("session {:#?}", session);

        if is_client_packet && !tcp_packet.bytes.is_empty() {
            let message = DaemonTcp::Data(TcpData {
                bytes: tcp_packet.bytes,
                connection_id: session.id,
            });
            self.send_message_to_clients(session.clients.iter(), message)
                .await?;
        }

        if is_closed_connection(tcp_flags) {
            self.index_allocator.free_index(session.id);
            self.connection_id_to_tcp_identifier.remove(&session.id);
            let message = DaemonTcp::Close(TcpClose {
                connection_id: session.id,
            });

            debug!(
                "TcpConnectionSniffer::handle_packet -> message {:#?}",
                message
            );

            self.send_message_to_clients(session.clients.iter(), message)
                .await?;
        } else {
            self.sessions.insert(identifier, session);
        }

        trace!("TcpConnectionSniffer::handle_packet -> finished");
        Ok(())
    }
}
