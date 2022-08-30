use std::{
    collections::{HashMap, HashSet},
    hash::{Hash, Hasher},
    net::{IpAddr, Ipv4Addr},
    path::PathBuf,
};

use futures::StreamExt;
use mirrord_protocol::{
    tcp::{DaemonTcp, NewTcpConnection, TcpClose, TcpData},
    ConnectionId, Port,
};
use pcap::{Active, Capture, Device, Linktype, PacketCodec, PacketStream};
use pnet::packet::{
    ethernet::{EtherTypes, EthernetPacket},
    ip::IpNextHeaderProtocols,
    ipv4::Ipv4Packet,
    tcp::{TcpFlags, TcpPacket},
    Packet,
};
use tokio::{
    select,
    sync::mpsc::{Receiver, Sender},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, trace, warn};

use crate::{
    error::AgentError,
    runtime::set_namespace,
    util::{ClientID, IndexAllocator, Subscriptions},
};

const DUMMY_BPF: &str =
    "tcp dst port 1 and tcp src port 1 and dst host 8.1.2.3 and src host 8.1.2.3";

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
    clients: HashSet<ClientID>,
}

type TCPSessionMap = HashMap<TcpSessionIdentifier, TCPSession>;

fn is_new_connection(flags: u16) -> bool {
    0 != (flags & TcpFlags::SYN) && 0 == (flags & (TcpFlags::ACK | TcpFlags::RST | TcpFlags::FIN))
}

fn is_closed_connection(flags: u16) -> bool {
    0 != (flags & (TcpFlags::FIN | TcpFlags::RST))
}

#[derive(Debug, Clone)]
pub struct TcpManagerCodec;

impl PacketCodec for TcpManagerCodec {
    type Item = Vec<u8>;

    fn decode(&mut self, packet: pcap::Packet) -> Self::Item {
        packet.data.to_vec()
    }
}

fn prepare_sniffer(interface: String) -> Result<Capture<Active>, AgentError> {
    debug!("prepare_sniffer -> Preparing interface.");

    let interface_names_match = |iface: &Device| iface.name == interface;
    let interfaces = Device::list()?;

    let interface = interfaces
        .into_iter()
        .find(interface_names_match)
        .ok_or_else(|| AgentError::NotFound("Interface not found!".to_string()))?;

    let mut capture = Capture::from_device(interface)?
        .immediate_mode(true)
        .open()?;

    capture.set_datalink(Linktype::ETHERNET)?;
    // Set a dummy filter that shouldn't capture anything. This makes the code easier.
    capture.filter(DUMMY_BPF, true)?;
    capture = capture.setnonblock()?;

    Ok(capture)
}

#[derive(Debug)]
struct TcpPacketData {
    bytes: Vec<u8>,
    flags: u16,
}

fn get_tcp_packet(eth_packet: Vec<u8>) -> Option<(TcpSessionIdentifier, TcpPacketData)> {
    let eth_packet = EthernetPacket::new(&eth_packet[..])?;
    debug!("get_tcp_packet_start");
    let ip_packet = match eth_packet.get_ethertype() {
        EtherTypes::Ipv4 => Ipv4Packet::new(eth_packet.payload())?,
        _ => return None,
    };
    debug!("ip_packet");
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
    debug!("identifier {identifier:?}");
    Some((
        identifier,
        TcpPacketData {
            flags: tcp_packet.get_flags(),
            bytes: tcp_packet.payload().to_vec(),
        },
    ))
}
/// Build a filter of format: "tcp port (80 or 443 or 50 or 90)"
fn format_bpf(ports: &[u16]) -> String {
    format!(
        "tcp port ({})",
        ports
            .iter()
            .map(|p| p.to_string())
            .collect::<Vec<String>>()
            .join(" or ")
    )
}

#[derive(Debug)]
enum SnifferCommands {
    NewAgent(Sender<DaemonTcp>),
    Subscribe(Port),
    UnsubscribePort(Port),
    UnsubscribeConnection(ConnectionId),
    AgentClosed,
}

#[derive(Debug)]
pub struct SnifferCommand {
    client_id: ClientID,
    command: SnifferCommands,
}

pub struct TCPSnifferAPI {
    client_id: ClientID,
    sender: Sender<SnifferCommand>,
    pub receiver: Receiver<DaemonTcp>,
}

impl TCPSnifferAPI {
    pub async fn new(
        client_id: ClientID,
        sniffer_sender: Sender<SnifferCommand>,
        receiver: Receiver<DaemonTcp>,
        tcp_sender: Sender<DaemonTcp>,
    ) -> Result<TCPSnifferAPI, AgentError> {
        sniffer_sender
            .send(SnifferCommand {
                client_id,
                command: SnifferCommands::NewAgent(tcp_sender),
            })
            .await?;
        Ok(Self {
            client_id,
            sender: sniffer_sender,
            receiver,
        })
    }

    pub async fn subscribe(&mut self, port: Port) -> Result<(), AgentError> {
        self.sender
            .send(SnifferCommand {
                client_id: self.client_id,
                command: SnifferCommands::Subscribe(port),
            })
            .await
            .map_err(From::from)
    }

    pub async fn connection_unsubscribe(
        &mut self,
        connection_id: ConnectionId,
    ) -> Result<(), AgentError> {
        self.sender
            .send(SnifferCommand {
                client_id: self.client_id,
                command: SnifferCommands::UnsubscribeConnection(connection_id),
            })
            .await
            .map_err(From::from)
    }

    pub async fn port_unsubscribe(&mut self, port: Port) -> Result<(), AgentError> {
        self.sender
            .send(SnifferCommand {
                client_id: self.client_id,
                command: SnifferCommands::UnsubscribePort(port),
            })
            .await
            .map_err(From::from)
    }

    pub async fn recv(&mut self) -> Option<DaemonTcp> {
        self.receiver.recv().await
    }
}

impl Drop for TCPSnifferAPI {
    fn drop(&mut self) {
        self.sender
            .try_send(SnifferCommand {
                client_id: self.client_id,
                command: SnifferCommands::AgentClosed,
            })
            .unwrap();
    }
}

pub struct TCPConnectionSniffer {
    port_subscriptions: Subscriptions<Port, ClientID>,
    receiver: Receiver<SnifferCommand>,
    client_senders: HashMap<ClientID, Sender<DaemonTcp>>,
    stream: PacketStream<Active, TcpManagerCodec>,
    sessions: TCPSessionMap,
    //todo: impl drop for index allocator and connection id..
    connection_id_to_tcp_identifier: HashMap<ConnectionId, TcpSessionIdentifier>,
    index_allocator: IndexAllocator<ConnectionId>,
}

impl TCPConnectionSniffer {
    pub async fn run(mut self, cancel_token: CancellationToken) -> Result<(), AgentError> {
        loop {
            select! {
                command = self.receiver.recv() => {
                    if let Some(command) = command {
                        self.handle_command(command).await?;
                    } else { break; }
                },
                packet = self.stream.next() => {
                    if let Some(packet) = packet {
                        self.handle_packet(packet?).await?;
                    } else { break; }
                }
                _ = cancel_token.cancelled() => {
                    break;
                }
            }
        }
        debug!("TCPConnectionSniffer exiting");
        Ok(())
    }

    pub async fn new(
        receiver: Receiver<SnifferCommand>,
        pid: Option<u64>,
        interface: String,
    ) -> Result<TCPConnectionSniffer, AgentError> {
        if let Some(pid) = pid {
            let namespace = PathBuf::from("/proc")
                .join(PathBuf::from(pid.to_string()))
                .join(PathBuf::from("ns/net"));

            set_namespace(namespace).unwrap();
        }

        debug!("preparing sniffer");
        let sniffer = prepare_sniffer(interface)?;
        let codec = TcpManagerCodec {};
        let stream = sniffer.stream(codec)?;
        Ok(TCPConnectionSniffer {
            receiver,
            stream,
            port_subscriptions: Subscriptions::new(),
            client_senders: HashMap::new(),
            sessions: TCPSessionMap::new(),
            //todo: impl drop for index allocator and connection id..
            connection_id_to_tcp_identifier: HashMap::new(),
            index_allocator: IndexAllocator::new(),
        })
    }

    pub async fn start(
        receiver: Receiver<SnifferCommand>,
        pid: Option<u64>,
        interface: String,
        cancel_token: CancellationToken,
    ) -> Result<(), AgentError> {
        let sniffer = Self::new(receiver, pid, interface).await?;
        sniffer.run(cancel_token).await
    }

    fn handle_new_client(&mut self, client_id: ClientID, sender: Sender<DaemonTcp>) {
        self.client_senders.insert(client_id, sender);
    }

    async fn handle_subscribe(
        &mut self,
        client_id: ClientID,
        port: Port,
    ) -> Result<(), AgentError> {
        self.port_subscriptions.subscribe(client_id, port);
        self.update_sniffer()?;
        self.send_message_to_client(&client_id, DaemonTcp::Subscribed)
            .await
    }

    fn handle_client_closed(&mut self, client_id: ClientID) -> Result<(), AgentError> {
        self.client_senders.remove(&client_id);
        self.port_subscriptions.remove_client(client_id);
        self.update_sniffer()
    }

    fn update_sniffer(&mut self) -> Result<(), AgentError> {
        let ports = self.port_subscriptions.get_subscribed_topics();
        let sniffer = self.stream.capture_mut();

        if ports.is_empty() {
            debug!("packet_worker -> empty ports, setting dummy bpf");
            sniffer.filter(DUMMY_BPF, true)?
        } else {
            let bpf = format_bpf(&ports);
            debug!("packet_worker -> setting bpf to {:?}", &bpf);

            sniffer.filter(&bpf, true)?
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
        clients: impl Iterator<Item = &ClientID>,
        message: DaemonTcp,
    ) -> Result<(), AgentError> {
        trace!(
            "TcpConnectionSniffer::send_message_to_clients -> message {:#?}",
            message
        );

        for client_id in clients {
            self.send_message_to_client(client_id, message.clone())
                .await?;
        }
        Ok(())
    }

    async fn send_message_to_client(
        &mut self,
        client_id: &ClientID,
        message: DaemonTcp,
    ) -> Result<(), AgentError> {
        if let Some(sender) = self.client_senders.get(client_id) {
            sender.send(message).await.map_err(|err| {
                warn!("failed to send message to client {}", client_id);
                let _ = self.handle_client_closed(*client_id);
                err
            })?;
        }
        Ok(())
    }

    async fn handle_packet(&mut self, eth_packet: Vec<u8>) -> Result<(), AgentError> {
        trace!(
            "TcpConnectionSniffer::handle_packet -> eth_packet {:#?}",
            eth_packet.len()
        );

        let (identifier, tcp_packet) = match get_tcp_packet(eth_packet) {
            Some(res) => res,
            None => return Ok(()),
        };

        let dest_port = identifier.dest_port;
        let source_port = identifier.source_port;
        let tcp_flags = tcp_packet.flags;
        debug!("TcpConnectionSniffer::handle_packet -> dest_port {:#?} | source_port {:#?} | tcp_flags {:#?}", dest_port, source_port, tcp_flags);

        let is_client_packet = self.qualified_port(dest_port);
        debug!(
            "TcpConnectionSniffer::handle_packet -> is_client_packet {:#?}",
            is_client_packet
        );

        let session = match self.sessions.remove(&identifier) {
            Some(session) => session,
            None => {
                if !is_new_connection(tcp_flags) {
                    if tcp_flags == 24 {
                        debug!("handle_packet -> wiwiwi {:?}", &tcp_packet);
                    }
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
                debug!(
                    "TcpConnectionSniffer::handle_packet -> client_ids {:#?}",
                    client_ids
                );

                let message = DaemonTcp::NewConnection(NewTcpConnection {
                    destination_port: dest_port,
                    source_port,
                    connection_id: id,
                    address: IpAddr::V4(identifier.source_addr),
                });
                debug!(
                    "TcpConnectionSniffer::handle_packet -> message {:#?}",
                    message
                );

                self.send_message_to_clients(client_ids.iter(), message)
                    .await?;

                self.connection_id_to_tcp_identifier.insert(id, identifier);

                TCPSession {
                    id,
                    clients: client_ids.into_iter().collect(),
                }
            }
        };
        debug!(
            "TcpConnectionSniffer::handle_packet -> session {:#?}",
            session
        );

        if is_client_packet && !tcp_packet.bytes.is_empty() {
            let message = DaemonTcp::Data(TcpData {
                bytes: tcp_packet.bytes,
                connection_id: session.id,
            });

            debug!(
                "TcpConnectionSniffer::handle_packet -> message {:#?}",
                message
            );

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
