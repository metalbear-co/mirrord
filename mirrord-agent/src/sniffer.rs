use std::{
    collections::{HashMap, HashSet},
    hash::{Hash, Hasher},
    net::{IpAddr, Ipv4Addr},
    path::PathBuf,
};

use futures::StreamExt;
use mirrord_protocol::{
    tcp::{DaemonTcp, NewTcpConnection, TcpClose, TcpData},
    ConnectionID,
};
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
use tracing::{debug, error, warn};

use crate::{
    error::AgentError,
    runtime::set_namespace,
    util::{IndexAllocator, Subscriptions},
    AgentID,
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

struct TCPSession {
    id: ConnectionID,
    agents: HashSet<AgentID>,
}

type TCPSessionMap = HashMap<TcpSessionIdentifier, TCPSession>;

fn is_new_connection(flags: u16) -> bool {
    flags == TcpFlags::SYN
}

fn is_closed_connection(flags: u16) -> bool {
    0 != (flags & (TcpFlags::FIN | TcpFlags::RST))
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

enum SnifferCommands {
    NewAgent(Sender<DaemonTcp>),
    Subscribe(Port),
    UnsubscribePort(Port),
    UnsubscribeConnection(ConnectionID),
    AgentClosed,
}

#[derive(Debug)]
struct SnifferCommand {
    agent_id: AgentID,
    command: SnifferCommands,
}

pub struct TCPSnifferAPI<State> {
    agent_id: AgentID,
    sender: Sender<SnifferCommand>,
    state: std::marker::PhantomData<State>,
    pub receiver: Receiver<DaemonTcp>,
}
struct Disabled;
struct Enabled;

impl TCPSnifferAPI {
    pub fn new(
        agent_id: AgentID,
        sender: Sender<SnifferCommand>,
        receiver: Receiver<DaemonTcp>,
    ) -> TCPSnifferAPI<Disabled> {
        Self {
            agent_id,
            sender,
            state: std::marker::PhantomData,
            receiver,
        }
    }
}

impl TCPSnifferAPI<Disabled> {
    pub async fn enable(
        mut self,
        sender: Sender<DaemonTcp>,
    ) -> Result(TCPSnifferAPI<Enabled>, AgentError) {
        self.sender
            .send(SnifferCommand {
                agent_id: self.agent_id,
                command: SnifferCommands::NewAgent(NewAgent { sender }),
            })
            .await?;
        Ok(self)
    }
}

impl TCPSnifferAPI<Enabled> {
    pub async fn subscribe(&mut self, port: Port) -> Result<(), AgentError> {
        self.sender
            .send(SnifferCommand {
                agent_id: self.agent_id,
                command: SnifferCommands::Subscribe(Subscribe { port }),
            })
            .await
    }

    pub async fn connection_unsubscribe(
        &mut self,
        connection_id: ConnectionID,
    ) -> Result<(), AgentError> {
        self.sender
            .send(SnifferCommand {
                agent_id: self.agent_id,
                command: SnifferCommands::UnsubscribeConnection(UnsubscribeConnection {
                    connection_id,
                }),
            })
            .await
    }

    pub async fn port_unsubscribe(&mut self, port: Port) -> Result<(), AgentError> {
        self.sender
            .send(SnifferCommand {
                agent_id: self.agent_id,
                command: SnifferCommands::UnsubscribePort(UnsubscribePort { port }),
            })
            .await
    }
}

impl Drop for TCPSnifferAPI<Enabled> {
    fn drop(&mut self) {
        self.sender
            .blocking_send(SnifferCommand {
                agent_id: self.agent_id,
                command: SnifferCommands::AgentClosed,
            })
            .unwrap();
    }
}

pub struct TCPConnectionSniffer {
    port_subscriptions: Subscriptions<Port, AgentID>,
    receiver: Receiver<SnifferCommand>,
    agent_senders: HashMap<AgentID, Sender<DaemonTcp>>,
    stream: PacketStream<Active, TcpManagerCodec>,
    sessions: TCPSessionMap,
    index_allocator: IndexAllocator<ConnectionID>,
}

impl TCPConnectionSniffer {
    pub async fn run(mut self) -> Result<()> {
        loop {
            select! {
                command = self.receiver.next() => {
                    self.handle_command(command).await?;
                }
            }
        }
        Ok(())
    }

    pub async fn new(
        receiver: Receiver<SnifferCommand>,
        pid: Option<u64>,
        interface: String,
    ) -> Result<TCPConnectionSniffer> {
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
            ..Default::default()
        })
    }

    pub async fn start(
        receiver: Receiver<SnifferCommand>,
        pid: Option<u64>,
        interface: String,
    ) -> Result<()> {
        let sniffer = Self::new(receiver, pid, interface).await?;
        sniffer.run().await
    }

    fn handle_new_agent(&mut self, agent_id: AgentID, sender: Sender<DaemonTcp>) {
        self.agent_senders.insert(agent_id, sender);
    }

    fn handle_subscribe(&mut self, agent_id: AgentID, port: Port) -> Result<()> {
        self.port_subscriptions.subscribe(port, agent_id);
        self.update_sniffer()
    }

    fn handle_agent_closed(&mut self, agent_id: AgentID) -> Result<()> {
        self.agent_senders.remove(&agent_id);
        self.port_subscriptions.remove_client(agent_id);
        self.update_sniffer()
    }

    fn update_sniffer(&mut self) -> Result<()> {
        let ports = self.port_subscriptions.get_subscribed_topics();
        let sniffer = self.stream.inner_mut();

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

    async fn handle_command(&mut self, command: SnifferCommand) -> Result<()> {
        match command {
            SnifferCommand {
                agent_id,
                command: SnifferCommands::NewAgent(NewAgent { sender }),
            } => {
                self.handle_new_agent(agent_id, sender);
            }
            SnifferCommand {
                agent_id,
                command: SnifferCommands::Subscribe(Subscribe { port }),
            } => {
                self.handle_subscribe(agent_id, port)?;
            }
            SnifferCommand {
                agent_id,
                command: SnifferCommands::AgentClosed,
            } => {
                self.handle_agent_closed(agent_id)?;
            }
            SnifferCommand {
                agent_id,
                command:
                    SnifferCommands::UnsubscribeConnection(UnsubscribeConnection { connection_id }),
            } => {
                self.sessions
                    .get_mut(connection_id)
                    .map(|session| session.agents.remove(agent_id));
            }
            SnifferCommand {
                agent_id,
                command: SnifferCommands::UnsubscribePort(UnsubscribePort { port }),
            } => {
                self.port_subscriptions.unsubscribe(agent_id, port);
                self.update_sniffer()?;
            }
        }
        Ok(())
    }

    async fn send_message_to_agents(
        &mut self,
        agents: impl Iterator<Item = AgentID>,
        message: DaemonTcp,
    ) -> Result<()> {
        for agent_id in agents {
            if let Some(sender) = self.agent_senders.get(&agent_id) {
                sender.send(message.clone()).await.ok_or_else(|| {
                    warn!("failed to send message to agent {}", agent_id);
                    self.handle_agent_closed(agent_id)?;
                })?;
            }
        }
        Ok(())
    }

    async fn handle_packet(&mut self, eth_packet: &EthernetPacket) -> Result<()> {
        debug!("handle_packet -> handling eth_packet {:#?}", eth_packet);

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
                let agent_ids = self.port_subscriptions.get_topic_subscribers(dest_port);

                let message = DaemonTcp::NewConnection(NewTcpConnection {
                    destination_port: dest_port,
                    source_port,
                    connection_id: id,
                    address: IpAddr::V4(identifier.source_addr),
                });
                self.send_message_to_agents(agent_ids, message)?;
                TCPSession {
                    id,
                    agents: agent_ids,
                }
            }
        };

        if is_client_packet {
            let data = tcp_packet.payload();

            if !data.is_empty() {
                let message = DaemonTcp::Data(TcpData {
                    bytes: data.to_vec(),
                    connection_id: session.id,
                });
                self.send_message_to_agents(&session.agents, message)?;
            }
        }

        if is_closed_connection(tcp_flags) {
            self.index_allocator.free_index(session);

            let message = DaemonTcp::Close(TcpClose {
                connection_id: session,
            });
            self.send_message_to_agents(&session.agents, message)?;
        } else {
            self.sessions.insert(identifier, session);
        }

        Ok(())
    }
}
