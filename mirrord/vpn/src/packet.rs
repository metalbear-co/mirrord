use pnet_packet::{ip::IpNextHeaderProtocols, ipv4, tcp, udp, MutablePacket as _};

pub fn patch_packet_checksum(packet: &mut [u8]) {
    let Some(mut ipv4_packet) = ipv4::MutableIpv4Packet::new(packet) else {
        tracing::warn!("packet wasn't ipv4");

        return;
    };

    let source = ipv4_packet.get_source();
    let destination = ipv4_packet.get_destination();

    match ipv4_packet.get_next_level_protocol() {
        IpNextHeaderProtocols::Udp => {
            let Some(mut udp_packet) = udp::MutableUdpPacket::new(ipv4_packet.payload_mut()) else {
                return;
            };

            udp_packet.set_checksum(udp::ipv4_checksum(
                &udp_packet.to_immutable(),
                &source,
                &destination,
            ))
        }
        IpNextHeaderProtocols::Tcp => {
            let Some(mut tcp_packet) = tcp::MutableTcpPacket::new(ipv4_packet.payload_mut()) else {
                return;
            };

            tcp_packet.set_checksum(tcp::ipv4_checksum(
                &tcp_packet.to_immutable(),
                &source,
                &destination,
            ))
        }
        _ => {}
    }
}
