use std::{
    collections::{HashMap, HashSet},
    hash::{Hash, Hasher},
    net::{IpAddr, Ipv4Addr},
    path::PathBuf,
};

use futures::StreamExt;
use mirrord_protocol::tcp::{NewTcpConnection, TcpClose, TcpData};
use pcap::{stream::PacketCodec, Active, Capture, Device, Linktype};
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
use tracing::{debug, error};

use crate::{error::AgentError, runtime::set_namespace, util::IndexAllocator, AgentID};

const DUMMY_BPF: &str =
    "tcp dst port 1 and tcp src port 1 and dst host 8.1.2.3 and src host 8.1.2.3";

type ConnectionID = u16;

#[derive(Debug)]
pub enum SnifferCommand {
    SetPorts(Vec<u16>),
    Close,
}

#[derive(Debug)]
pub enum SnifferOutput {
    NewTcpConnection(NewTcpConnection),
    TcpClose(TcpClose),
    TcpData(TcpData),
}

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

type Session = ConnectionID;
type SessionMap = HashMap<TcpSessionIdentifier, Session>;

fn is_new_connection(flags: u16) -> bool {
    flags == TcpFlags::SYN
}

fn is_closed_connection(flags: u16) -> bool {
    0 != (flags & (TcpFlags::FIN | TcpFlags::RST))
}

struct ConnectionManager {
    sessions: SessionMap,
    index_allocator: IndexAllocator<ConnectionID>,
    ports: HashSet<u16>,
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

impl ConnectionManager {
    fn new() -> Self {
        ConnectionManager {
            sessions: HashMap::new(),
            index_allocator: IndexAllocator::new(),
            ports: HashSet::new(),
        }
    }

    fn qualified_port(&self, port: u16) -> bool {
        self.ports.contains(&port)
    }

    fn set_ports(&mut self, ports: &[u16]) {
        self.ports = HashSet::from_iter(ports.iter().cloned())
    }

    /// Called by `packet_worker` to convert data packets into `SnifferOutput`s.
    // TODO: Looks eerily similar to a custom `Into` implementation, or could be superseded by some
    // `map` operation. It feels weird to return an `Option<Vec<T>>`, as an empty `Vec` is
    // arguably the same as `None`.
    fn handle_packet(&mut self, eth_packet: &EthernetPacket) -> Option<Vec<SnifferOutput>> {
        debug!("handle_packet -> handling eth_packet {:#?}", eth_packet);
        let mut messages = vec![];

        let ip_packet = match eth_packet.get_ethertype() {
            EtherTypes::Ipv4 => Ipv4Packet::new(eth_packet.payload())?,
            _ => return None,
        };

        let tcp_packet = match ip_packet.get_next_level_protocol() {
            IpNextHeaderProtocols::Tcp => TcpPacket::new(ip_packet.payload())?,
            _ => return None,
        };

        let dest_port = tcp_packet.get_destination();
        let tcp_flags = tcp_packet.get_flags();
        let source_port = tcp_packet.get_source();

        let identifier = TcpSessionIdentifier {
            source_addr: ip_packet.get_source(),
            dest_addr: ip_packet.get_destination(),
            source_port,
            dest_port,
        };

        let is_client_packet = self.qualified_port(dest_port);

        let session = match self.sessions.remove(&identifier) {
            Some(session) => session,
            None => {
                if !is_new_connection(tcp_flags) {
                    return None;
                }
                if !is_client_packet {
                    return None;
                }

                let id = self.index_allocator.next_index().or_else(|| {
                    error!("connection index exhausted, dropping new connection");
                    None
                })?;
                messages.push(SnifferOutput::NewTcpConnection(NewTcpConnection {
                    destination_port: dest_port,
                    source_port,
                    connection_id: id,
                    address: IpAddr::V4(identifier.source_addr),
                }));
                id
            }
        };

        if is_client_packet {
            let data = tcp_packet.payload();

            if !data.is_empty() {
                messages.push(SnifferOutput::TcpData(TcpData {
                    bytes: data.to_vec(),
                    connection_id: session,
                }));
            }
        }

        if is_closed_connection(tcp_flags) {
            self.index_allocator.free_index(session);

            messages.push(SnifferOutput::TcpClose(TcpClose {
                connection_id: session,
            }));
        } else {
            self.sessions.insert(identifier, session);
        }

        Some(messages)
    }
}

pub struct TcpManagerCodec {}

impl PacketCodec for TcpManagerCodec {
    type Type = Vec<u8>;

    fn decode(&mut self, packet: pcap::Packet) -> Result<Self::Type, pcap::Error> {
        Ok(packet.data.to_vec())
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

pub async fn packet_worker(
    sniffer_output_tx: Sender<SnifferOutput>,
    mut sniffer_command_rx: Receiver<SnifferCommand>,
    interface: String,
    pid: Option<u64>,
) -> Result<(), AgentError> {
    debug!("packet_worker -> setting namespace");

    if let Some(pid) = pid {
        let namespace = PathBuf::from("/proc")
            .join(PathBuf::from(pid.to_string()))
            .join(PathBuf::from("ns/net"));

        set_namespace(namespace).unwrap();
    }

    debug!("preparing sniffer");
    let sniffer = prepare_sniffer(interface)?;
    let codec = TcpManagerCodec {};
    let mut connection_manager = ConnectionManager::new();
    let mut sniffer_stream = sniffer.stream(codec)?;

    loop {
        select! {
            // Converts data packets into `SnifferOutput`s, then sends these to be handled by ?.
            Some(Ok(packet)) = sniffer_stream.next() => {
                debug!("packet_worker -> sniffer_stream has a packet.");

                let sniffer_messages = EthernetPacket::new(&packet)
                    .and_then(|packet| connection_manager.handle_packet(&packet))
                    .unwrap_or_default();

                debug!("packet_worker -> sniffer_messages");

                for sniffer_output in sniffer_messages.into_iter() {
                    sniffer_output_tx.send(sniffer_output).await?;
                }

            },
            sniffer_command = sniffer_command_rx.recv() => {
                match sniffer_command {
                    Some(SnifferCommand::SetPorts(ports)) => {
                        debug!("packet_worker -> setting ports {:?}", &ports);

                        connection_manager.set_ports(&ports);
                        let sniffer = sniffer_stream.inner_mut();

                        if ports.is_empty() {
                            debug!("packet_worker -> empty ports, setting dummy bpf");
                            sniffer.filter(DUMMY_BPF, true)?
                        } else {
                            let bpf = format_bpf(&ports);
                            debug!("packet_worker -> setting bpf to {:?}", &bpf);

                            sniffer.filter(&bpf, true)?
                        };

                    },
                    Some(SnifferCommand::Close) | None => {
                        debug!("packet_worker -> sniffer closed");
                        break;
                    }
                }
            },
            _ = sniffer_output_tx.closed() => {
                debug!("packet_worker -> closing due to sniffer_tx closed");
                break;
            },

        }
    }
    debug!("packet_worker -> finished");
    Ok(())
}

pub struct NewPeer {
    sender: Sender<DaemonTcp>,
    receiver: Receiver<LayerTcp>,
}

type NewPeerReceiver = Receiver<NewPeer>;

struct NewAgent {
    sender: Sender<DaemonTcp>
}

struct Subscribe {
    port: Port,
}

enum SnifferCommands {
    NewAgent(NewAgent),
    Subscribe(Subscribe),
    AgentClosed
}

#[derive(Debug)]
struct SnifferCommand {
    agent_id: AgentID,
    command: SnifferCommands
}

struct TCPSnifferAPI<State> {
    agent_id: AgentID,
    sender: Sender<SnifferCommand>,
    state: std::marker::PhantomData<State>
}
struct Disabled;
struct Enabled;

impl TCPSnifferAPI {
    pub fn new(agent_id: AgentID, sender: Sender<SnifferCommand>) -> TCPSnifferAPI<Disabled> {
        Self { agent_id, sender, state: std::marker::PhantomData }
    }
}

impl TCPSnifferAPI<Disabled> {
    pub async fn enable(mut self, sender: Sender<DaemonTcp>) -> Result(TCPSnifferAPI<Enabled>, AgentError) {
        self.sender.send(SnifferCommand {
            agent_id: self.agent_id,
            command: SnifferCommands::NewAgent(NewAgent {
                sender
            })
        }).await?;
        Ok(self)
    }
}

impl TCPSnifferAPI<Enabled> {
    pub async fn subscribe(&mut self, port: Port) -> Result((), AgentError) {
        self.sender.send(SnifferCommand {
            agent_id: self.agent_id,
            command: SnifferCommands::Subscribe(Subscribe {
                port
            })
        }).await?;
        Ok(())
    }
}

impl Drop for TCPSnifferAPI<Enabled> {
    fn drop(&mut self) {
        self.sender.blocking_send(SnifferCommand {
            agent_id: self.agent_id,
            command: SnifferCommands::AgentClosed
        }).unwrap();
    }
}


struct TCPConnectionSniffer {
    port_subscriptions: Subscriptions<Port, AgentID>,
    reciever: Receiver<SnifferCommand>,
    agent_senders: HashMap<AgentID, Sender<DaemonTcp>>,
}

impl TCPConnectionSniffer {

    pub async fn run(mut self) -> Result<()> {
        loop {
            select! {
                command = self.reciever.next() => {
                    self.handle_command(command).await?;
                }
            }
        }       
        Ok(())
    }

    fn handle_new_agent(&mut self, agent_id: AgentID, sender: Sender<DaemonTcp>) {
        self.agent_senders.insert(agent_id, sender);
    }

    fn handle_subscribe(&mut self, agent_id: AgentID, port: Port) {
        self.port_subscriptions.subscribe(port, agent_id);
    }

    fn handle_agent_closed(&mut self, agent_id: AgentID) {
        self.agent_senders.remove(&agent_id);
        self.port_subscriptions.remove_client(agent_id);
    }

    async fn handle_command(&mut self, command: SnifferCommand) -> Result<()> {
        match command {
            SnifferCommand {
                agent_id,
                command: SnifferCommands::NewAgent(NewAgent { sender })} => {
                    self.handle_new_agent(agent_id, sender);
                },
            SnifferCommand {
                agent_id,
                command: SnifferCommands::Subscribe(Subscribe { port })
            } => {
                self.handle_subscribe(agent_id, port);
            },
            SnifferCommand {
                agent_id,
                command: SnifferCommands::AgentClosed
            } => {
                self.handle_agent_closed(agent_id);
            }
        }
        Ok(())
    }
}