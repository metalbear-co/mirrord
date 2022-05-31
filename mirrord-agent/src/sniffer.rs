use std::{
    collections::{HashMap, HashSet},
    hash::{Hash, Hasher},
    net::{IpAddr, Ipv4Addr},
    path::PathBuf,
};

use anyhow::{anyhow, Result};
use futures::StreamExt;
use mirrord_protocol::{NewTCPConnection, TCPClose, TCPData};
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

use crate::{
    runtime::{set_namespace, Runtime},
    util::IndexAllocator,
};

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
    NewTCPConnection(NewTCPConnection),
    TCPClose(TCPClose),
    TCPData(TCPData),
}

#[derive(Debug, Eq, Copy, Clone)]
pub struct TCPSessionIdentifier {
    source_addr: Ipv4Addr,
    dest_addr: Ipv4Addr,
    source_port: u16,
    dest_port: u16,
}

impl PartialEq for TCPSessionIdentifier {
    /// It's the same session if 4 tuple is same/opposite.
    fn eq(&self, other: &TCPSessionIdentifier) -> bool {
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

impl Hash for TCPSessionIdentifier {
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
type SessionMap = HashMap<TCPSessionIdentifier, Session>;

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

    fn handle_packet(&mut self, eth_packet: &EthernetPacket) -> Option<Vec<SnifferOutput>> {
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
        let identifier = TCPSessionIdentifier {
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
                messages.push(SnifferOutput::NewTCPConnection(NewTCPConnection {
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
                messages.push(SnifferOutput::TCPData(TCPData {
                    data: data.to_vec(),
                    connection_id: session,
                }));
            }
        }
        if is_closed_connection(tcp_flags) {
            self.index_allocator.free_index(session);
            messages.push(SnifferOutput::TCPClose(TCPClose {
                connection_id: session,
            }));
        } else {
            self.sessions.insert(identifier, session);
        }
        Some(messages)
    }
}

pub struct TCPManagerCodec {}

impl PacketCodec for TCPManagerCodec {
    type Type = Vec<u8>;

    fn decode(&mut self, packet: pcap::Packet) -> Result<Self::Type, pcap::Error> {
        Ok(packet.data.to_vec())
        // let res = match EthernetPacket::new(packet.data) {
        //     Some(packet) => self
        //         .connection_manager
        //         .handle_packet(&packet)
        //         .unwrap_or(vec![]),
        //     _ => vec![],
        // };
        // Ok(res)
    }
}

fn prepare_sniffer(interface: String) -> Result<Capture<Active>> {
    let interface_names_match = |iface: &Device| iface.name == interface;
    let interfaces = Device::list()?;
    let interface = interfaces
        .into_iter()
        .find(interface_names_match)
        .ok_or_else(|| anyhow!("Interface not found"))?;

    let mut cap = Capture::from_device(interface)?
        .immediate_mode(true)
        .open()?;
    cap.set_datalink(Linktype::ETHERNET)?;
    // Set a dummy filter that shouldn't capture anything. This makes the code easier.
    cap.filter(DUMMY_BPF, true)?;
    cap = cap.setnonblock()?;
    Ok(cap)
}

pub async fn packet_worker(
    tx: Sender<SnifferOutput>,
    mut rx: Receiver<SnifferCommand>,
    interface: String,
    container_id: Option<String>,
    container_runtime: Option<String>,
) -> Result<()> {
    debug!("setting namespace");

    let default_runtime = "containerd";
    let pid = match (container_id, container_runtime) {
        (Some(container_id), Some(container_runtime)) => {
            Runtime::get_container_pid(&container_id, &container_runtime)
                .await
                .ok()
        }
        (Some(container_id), None) => Runtime::get_container_pid(&container_id, default_runtime)
            .await
            .ok(),
        (None, Some(_)) => return Err(anyhow!("Container ID not specified")),
        _ => None,
    };

    if let Some(pid) = pid {
        let namespace = PathBuf::from("/proc").join(pid).join("ns/net");
        set_namespace(namespace).unwrap();
    }

    debug!("preparing sniffer");
    let sniffer = prepare_sniffer(interface)?;
    debug!("done prepare sniffer");
    let codec = TCPManagerCodec {};
    let mut connection_manager = ConnectionManager::new();
    let mut stream = sniffer.stream(codec)?;
    loop {
        select! {
            Some(Ok(packet)) = stream.next() => {
                    let messages = match EthernetPacket::new(&packet) {
                        Some(packet) =>
                            connection_manager
                            .handle_packet(&packet)
                            .unwrap_or_default(),
                        _ => vec![],
                    };
                    for message in messages.into_iter() {
                        tx.send(message).await?;
                    }

            },
            message = rx.recv() => {
                match message {
                    Some(SnifferCommand::SetPorts(ports)) => {
                        debug!("setting ports {:?}", &ports);
                        connection_manager.set_ports(&ports);
                        let sniffer = stream.inner_mut();
                        if ports.is_empty() {
                            debug!("empty ports, setting dummy bpf");
                            sniffer.filter(DUMMY_BPF, true)?
                        } else {
                            let bpf = format_bpf(&ports);
                            debug!("setting bpf to {:?}", &bpf);
                            sniffer.filter(&bpf, true)?
                        };

                    },
                    Some(SnifferCommand::Close) | None => {
                        debug!("sniffer closed");
                        break;
                    }
                }
            },
            _ = tx.closed() => {
                debug!("closing due to tx closed");
                break;
            },

        }
    }
    debug!("end of packet_worker");
    Ok(())
}
