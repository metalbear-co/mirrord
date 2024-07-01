use std::{
    collections::{hash_map::Entry, HashMap},
    fmt,
    future::Future,
    hash::{Hash, Hasher},
    io,
    net::{Ipv4Addr, SocketAddr},
    pin::Pin,
    task::{Context, Poll},
};

use futures::{stream::FuturesUnordered, StreamExt};
use mirrord_protocol::{MeshVendor, Port};
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
    sync::{
        broadcast,
        mpsc::{error::TrySendError, Receiver, Sender},
    },
};
use tokio_util::sync::CancellationToken;
use tracing::Level;

use self::messages::{SniffedConnection, SnifferCommand, SnifferCommandInner};
use crate::{
    error::AgentError,
    http::HttpVersion,
    util::{ClientId, Subscriptions},
};

pub(crate) mod api;
pub(crate) mod messages;

/// [`Future`] that resolves to [`ClientId`] when the [`TcpConnectionSniffer`] client drops their
/// [`TcpSnifferApi`](api::TcpSnifferApi).
struct ClientClosed {
    /// [`Sender`] used by [`TcpConnectionSniffer`] to send data to the client.
    /// Here used only to poll [`Sender::closed`].
    client_tx: Sender<SniffedConnection>,
    /// Id of the client.
    client_id: ClientId,
}

impl Future for ClientClosed {
    type Output = ClientId;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let client_id = self.client_id;

        let future = std::pin::pin!(self.get_mut().client_tx.closed());
        std::task::ready!(future.poll(cx));

        Poll::Ready(client_id)
    }
}

#[derive(Debug, Eq, Copy, Clone)]
pub(crate) struct TcpSessionIdentifier {
    /// The remote address that is sending a packet to the impersonated pod.
    ///
    /// ## Details
    ///
    /// If you were to `curl {impersonated_pod_ip}:{port}`, this would be the address of whoever
    /// is making the request.
    pub(crate) source_addr: Ipv4Addr,

    /// Local address of the impersonated pod.
    ///
    /// ## Details
    ///
    /// You can get this IP by checking `kubectl get pod -o wide`.
    ///
    /// ```sh
    /// $ kubectl get pod -o wide
    /// NAME        READY   STATUS    IP
    /// happy-pod   1/1     Running   1.2.3.4   
    /// ```
    pub(crate) dest_addr: Ipv4Addr,
    pub(crate) source_port: u16,
    pub(crate) dest_port: u16,
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

type TCPSessionMap = HashMap<TcpSessionIdentifier, broadcast::Sender<Vec<u8>>>;

const fn is_new_connection(flags: u8) -> bool {
    0 != (flags & TcpFlags::SYN) && 0 == (flags & (TcpFlags::ACK | TcpFlags::RST | TcpFlags::FIN))
}

fn is_closed_connection(flags: u8) -> bool {
    0 != (flags & (TcpFlags::FIN | TcpFlags::RST))
}

/// Connects to a remote address (`8.8.8.8:53`) so we can find which network interface to use.
///
/// Used when no `user_interface` is specified in [`prepare_sniffer`] to prevent mirrord from
/// defaulting to the wrong network interface (`eth0`), as sometimes the user's machine doesn't have
/// it available (i.e. their default network is `enp2s0`).
#[tracing::instrument(level = "trace")]
async fn resolve_interface() -> io::Result<Option<String>> {
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
#[tracing::instrument(level = Level::DEBUG, err)]
async fn prepare_sniffer(
    network_interface: Option<String>,
    mesh: Option<MeshVendor>,
) -> Result<RawCapture, AgentError> {
    // Priority is whatever the user set as an option to mirrord, then we check if we're in a mesh
    // to use `lo` interface, otherwise we try to get the appropriate interface.
    let interface = match network_interface.or_else(|| mesh.map(|_| "lo".to_string())) {
        Some(interface) => interface,
        None => resolve_interface()
            .await?
            .unwrap_or_else(|| "eth0".to_string()),
    };

    tracing::debug!(
        resolved_interface = interface,
        "Resolved raw capture interface"
    );

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
    flags: u8,
}

#[tracing::instrument(skip(eth_packet), level = Level::TRACE, fields(bytes = %eth_packet.len()))]
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

    tracing::trace!(session_identifier = ?identifier, "Got TCP packet");

    Some((
        identifier,
        TcpPacketData {
            flags: tcp_packet.get_flags(),
            bytes: tcp_packet.payload().to_vec(),
        },
    ))
}

pub(crate) struct TcpConnectionSniffer {
    command_rx: Receiver<SnifferCommand>,
    raw_capture: RawCapture,

    port_subscriptions: Subscriptions<Port, ClientId>,
    sessions: TCPSessionMap,

    client_txs: HashMap<ClientId, Sender<SniffedConnection>>,
    clients_closed: FuturesUnordered<ClientClosed>,
}

impl fmt::Debug for TcpConnectionSniffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TcpConnectionSniffer")
            .field("clients", &self.client_txs.keys())
            .field("port_subscriptions", &self.port_subscriptions)
            .field("open_tcp_sessions", &self.sessions.keys())
            .finish()
    }
}

impl TcpConnectionSniffer {
    pub const TASK_NAME: &'static str = "Sniffer";

    /// Runs the sniffer loop, capturing packets.
    #[tracing::instrument(level = Level::DEBUG, skip(cancel_token), err)]
    pub async fn start(mut self, cancel_token: CancellationToken) -> Result<(), AgentError> {
        loop {
            select! {
                command = self.command_rx.recv() => {
                    let Some(command) = command else {
                        tracing::debug!("command channel closed, exiting");
                        break;
                    };

                    self.handle_command(command)?;
                },

                Some(client_id) = self.clients_closed.next() => {
                    self.handle_client_closed(client_id)?;
                }

                packet = self.raw_capture.next() => {
                    self.handle_packet(packet?)?;
                }

                _ = cancel_token.cancelled() => {
                    tracing::debug!("token cancelled, exiting");
                    break;
                }
            }
        }

        Ok(())
    }

    /// Creates and prepares a new [`TcpConnectionSniffer`] that uses BPF filters to capture network
    /// packets.
    ///
    /// The capture uses a network interface specified by the user, if there is none, then it tries
    /// to find a proper one by starting a connection. If this fails, we use "eth0" as a last
    /// resort.
    #[tracing::instrument(level = Level::TRACE, skip(command_rx), err)]
    pub async fn new(
        command_rx: Receiver<SnifferCommand>,
        network_interface: Option<String>,
        mesh: Option<MeshVendor>,
    ) -> Result<Self, AgentError> {
        let raw_capture = prepare_sniffer(network_interface, mesh).await?;

        Ok(Self {
            command_rx,
            raw_capture,

            port_subscriptions: Default::default(),
            sessions: TCPSessionMap::new(),

            client_txs: HashMap::new(),
            clients_closed: Default::default(),
        })
    }

    /// New layer is connecting to this agent sniffer.
    #[tracing::instrument(level = Level::TRACE, skip(sender))]
    fn handle_new_client(&mut self, client_id: ClientId, sender: Sender<SniffedConnection>) {
        self.client_txs.insert(client_id, sender.clone());
        self.clients_closed.push(ClientClosed {
            client_tx: sender.clone(),
            client_id,
        });
    }

    /// Removes the client with `client_id`, and also unsubscribes its port.
    /// Adjusts BPF filter if needed.
    #[tracing::instrument(level = Level::TRACE, err)]
    fn handle_client_closed(&mut self, client_id: ClientId) -> Result<(), AgentError> {
        self.client_txs.remove(&client_id);

        if self.port_subscriptions.remove_client(client_id) {
            self.update_packet_filter()?;
        }

        Ok(())
    }

    /// Updates BPF filter used by [`Self::raw_capture`] to match state of
    /// [`Self::port_subscriptions`].
    #[tracing::instrument(level = Level::TRACE, err)]
    fn update_packet_filter(&mut self) -> Result<(), AgentError> {
        let ports = self.port_subscriptions.get_subscribed_topics();

        let filter = if ports.is_empty() {
            tracing::trace!("No ports subscribed, setting dummy bpf");
            rawsocket::filter::build_drop_always()
        } else {
            rawsocket::filter::build_tcp_port_filter(&ports)
        };

        self.raw_capture.set_filter(filter)?;

        Ok(())
    }

    #[tracing::instrument(level = Level::TRACE, err)]
    fn handle_command(&mut self, command: SnifferCommand) -> Result<(), AgentError> {
        match command {
            SnifferCommand {
                client_id,
                command: SnifferCommandInner::NewClient(sender),
            } => {
                self.handle_new_client(client_id, sender);
            }

            SnifferCommand {
                client_id,
                command: SnifferCommandInner::Subscribe(port, tx),
            } => {
                if self.port_subscriptions.subscribe(client_id, port) {
                    self.update_packet_filter()?;
                }

                let _ = tx.send(());
            }

            SnifferCommand {
                client_id,
                command: SnifferCommandInner::UnsubscribePort(port),
            } => {
                if self.port_subscriptions.unsubscribe(client_id, port) {
                    self.update_packet_filter()?;
                }
            }
        }

        Ok(())
    }

    /// First it checks the `tcp_flags` with [`is_new_connection`], if that's not the case, meaning
    /// we have traffic from some existing connection from before mirrord started, then it tries to
    /// see if `bytes` contains an HTTP request of some sort. When an HTTP request is
    /// detected, then the agent should start mirroring as if it was a new connection.
    ///
    /// tl;dr: checks packet flags, or if it's an HTTP packet, then begins a new sniffing session.
    #[tracing::instrument(level = Level::TRACE, ret, skip(bytes), fields(bytes = bytes.len()), ret)]
    fn treat_as_new_session(tcp_flags: u8, bytes: &[u8]) -> bool {
        is_new_connection(tcp_flags)
            || matches!(
                HttpVersion::new(bytes),
                Some(HttpVersion::V1 | HttpVersion::V2)
            )
    }

    /// Handles Ethernet packet sniffed by [`Self::raw_capture`].
    #[tracing::instrument(level = Level::TRACE, ret, skip(self, eth_packet), fields(bytes = %eth_packet.len()))]
    fn handle_packet(&mut self, eth_packet: Vec<u8>) -> Result<(), AgentError> {
        let (identifier, tcp_packet) = match get_tcp_packet(eth_packet) {
            Some(res) => res,
            None => {
                // Not a TCP packet, so not interesting at all.
                return Ok(());
            }
        };

        tracing::trace!(
            destination_port = identifier.dest_port,
            source_port = identifier.source_port,
            tcp_flags = tcp_packet.flags,
            bytes = tcp_packet.bytes.len(),
            "TCP packet intercepted",
        );

        let data_tx = match self.sessions.entry(identifier) {
            Entry::Occupied(e) => e,
            Entry::Vacant(e) => {
                // Performs a check on the `tcp_flags` and on the packet contents to see if this
                // should be treated as a new connection.
                if !Self::treat_as_new_session(tcp_packet.flags, &tcp_packet.bytes) {
                    // Either it's an existing session, or some sort of existing traffic we don't
                    // care to start mirroring.
                    return Ok(());
                }

                let Some(client_ids) = self
                    .port_subscriptions
                    .get_topic_subscribers(identifier.dest_port)
                    .filter(|ids| !ids.is_empty())
                else {
                    return Ok(());
                };

                tracing::trace!(
                    ?client_ids,
                    "TCP packet should be treated as new session and start connections for clients"
                );

                let (data_tx, _) = broadcast::channel(512);

                for client_id in client_ids {
                    let Some(client_tx) = self.client_txs.get(client_id) else {
                        tracing::error!(
                            client_id,
                            destination_port = identifier.dest_port,
                            source_port = identifier.source_port,
                            tcp_flags = tcp_packet.flags,
                            bytes = tcp_packet.bytes.len(),
                            "Failed to find client while handling new sniffed TCP connection, this is a bug",
                        );

                        continue;
                    };

                    let connection = SniffedConnection {
                        session_id: identifier,
                        data: data_tx.subscribe(),
                    };

                    match client_tx.try_send(connection) {
                        Ok(()) => {}

                        Err(TrySendError::Closed(..)) => {
                            // Client closed.
                            // State will be cleaned up when `self.clients_closed` picks it up.
                        }

                        Err(TrySendError::Full(..)) => {
                            tracing::warn!(
                                client_id,
                                destination_port = identifier.dest_port,
                                source_port = identifier.source_port,
                                tcp_flags = tcp_packet.flags,
                                bytes = tcp_packet.bytes.len(),
                                "Client queue of new sniffed TCP connections is full, dropping",
                            );

                            continue;
                        }
                    }
                }

                e.insert_entry(data_tx)
            }
        };

        tracing::trace!("Resolved data broadcast channel");

        if !tcp_packet.bytes.is_empty() && data_tx.get().send(tcp_packet.bytes).is_err() {
            tracing::trace!("All data receivers are dead, dropping data broadcast sender");
            data_tx.remove();
            return Ok(());
        }

        if is_closed_connection(tcp_packet.flags) {
            tracing::trace!("TCP packet closes connection, dropping data broadcast channel");
            data_tx.remove();
        }

        Ok(())
    }
}
