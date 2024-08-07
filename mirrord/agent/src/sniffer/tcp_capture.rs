use std::{io, net::SocketAddr};

use nix::sys::socket::SockaddrStorage;
use pnet::packet::{
    ethernet::{EtherTypes, EthernetPacket},
    ip::IpNextHeaderProtocols,
    ipv4::Ipv4Packet,
    tcp::TcpPacket,
    Packet,
};
use rawsocket::{filter::SocketFilterProgram, RawCapture};
use tokio::net::UdpSocket;
use tracing::Level;

use super::{TcpPacketData, TcpSessionIdentifier};
use crate::error::AgentError;

/// Trait for structs that are able to sniff incoming Ethernet packets and filter TCP packets.
pub trait TcpCapture {
    /// Sets a filter for incoming Ethernet packets.
    fn set_filter(&mut self, filter: SocketFilterProgram) -> io::Result<()>;

    /// Returns the next sniffed TCP packet.
    async fn next(&mut self) -> io::Result<(TcpSessionIdentifier, TcpPacketData)>;
}

/// Implementor of [`TcpCapture`] that uses a raw OS socket and a BPF filter.
pub struct RawSocketTcpCapture {
    /// Raw OS socket.
    inner: RawCapture,
}

impl RawSocketTcpCapture {
    /// Creates a new instance. `network_interface` and `mesh` will be used to determine correct
    /// network interface for the raw OS socket.
    ///
    /// Returned instance initially uses a BPF filter that drops every packet.
    #[tracing::instrument(level = Level::DEBUG, err)]
    pub async fn new(network_interface: Option<String>, is_mesh: bool) -> Result<Self, AgentError> {
        // Priority is whatever the user set as an option to mirrord, then we check if we're in a
        // mesh to use `lo` interface, otherwise we try to get the appropriate interface.
        let interface = match network_interface.or_else(|| is_mesh.then(|| "lo".to_string())) {
            Some(interface) => interface,
            None => Self::resolve_interface()
                .await?
                .unwrap_or_else(|| "eth0".to_string()),
        };

        tracing::debug!(
            resolved_interface = interface,
            "Resolved raw capture interface"
        );

        let capture = RawCapture::from_interface_name(&interface)?;
        capture.set_filter(rawsocket::filter::build_drop_always())?;
        capture
            .ignore_outgoing()
            .map_err(AgentError::PacketIgnoreOutgoing)?;
        Ok(Self { inner: capture })
    }

    /// Connects to a remote address (`8.8.8.8:53`) so we can find which network interface to use.
    ///
    /// Used when no `user_interface` is specified in [`Self::new`] to prevent mirrord from
    /// defaulting to the wrong network interface (`eth0`), as sometimes the user's machine doesn't
    /// have it available (i.e. their default network is `enp2s0`).
    #[tracing::instrument(level = Level::DEBUG, err)]
    async fn resolve_interface() -> io::Result<Option<String>> {
        // Connect to a remote address so we can later get the default network interface.
        let temporary_socket = UdpSocket::bind("0.0.0.0:0").await?;
        temporary_socket.connect("8.8.8.8:53").await?;

        // Create comparison address here with `port: 0`, to match the network interface's address
        // of `sin_port: 0`.
        let local_address = SocketAddr::new(temporary_socket.local_addr()?.ip(), 0);
        let raw_local_address = SockaddrStorage::from(local_address);

        // Try to find an interface that matches the local ip we have.
        let usable_interface_name: Option<String> = nix::ifaddrs::getifaddrs()?.find_map(|iface| {
            (raw_local_address == iface.address?).then_some(iface.interface_name)
        });

        Ok(usable_interface_name)
    }

    /// Extracts TCP packet from the raw Ethernet packet given as bytes.
    /// If the given Ethernet packet is not TCP, returns [`None`].
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
}

impl TcpCapture for RawSocketTcpCapture {
    fn set_filter(&mut self, filter: SocketFilterProgram) -> io::Result<()> {
        self.inner.set_filter(filter)
    }

    async fn next(&mut self) -> io::Result<(TcpSessionIdentifier, TcpPacketData)> {
        loop {
            let raw = self.inner.next().await?;

            if let Some(tcp) = Self::get_tcp_packet(raw) {
                break Ok(tcp);
            }
        }
    }
}

#[cfg(test)]
pub mod test {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    use tokio::sync::mpsc::Receiver;

    use super::*;

    /// Implementor of [`TcpCapture`] that returns packets received from an
    /// [`mpsc`](tokio::sync::mpsc) channel.
    pub struct TcpPacketsChannel {
        pub times_filter_changed: Arc<AtomicUsize>,
        pub receiver: Receiver<(TcpSessionIdentifier, TcpPacketData)>,
    }

    impl TcpCapture for TcpPacketsChannel {
        /// Filter is ignored, we don't want to execute BPF programs in tests.
        fn set_filter(&mut self, _filter: SocketFilterProgram) -> io::Result<()> {
            self.times_filter_changed.fetch_add(1, Ordering::Relaxed);

            Ok(())
        }

        async fn next(&mut self) -> io::Result<(TcpSessionIdentifier, TcpPacketData)> {
            Ok(self.receiver.recv().await.expect("channel closed"))
        }
    }
}
