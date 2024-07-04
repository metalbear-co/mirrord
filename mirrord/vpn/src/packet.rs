use pnet_packet::{
    ethernet::{self, EtherTypes},
    ip::IpNextHeaderProtocols,
    ipv4, ipv6, tcp, udp, MutablePacket as _,
};

pub fn patch_packet_checksum(packet: &mut [u8]) {
    let Some(ethernet_packet) = ethernet::EthernetPacket::new(packet) else {
        tracing::warn!("packet wasn't ethernet");

        return;
    };

    match ethernet_packet.get_ethertype() {
        EtherTypes::Ipv6 => patch_ipv6_checksum(packet),
        _ => patch_ipv4_checksum(packet),
    }
}

fn patch_ipv4_checksum(packet: &mut [u8]) {
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

fn patch_ipv6_checksum(packet: &mut [u8]) {
    let Some(mut ipv6_packet) = ipv6::MutableIpv6Packet::new(packet) else {
        tracing::warn!("packet wasn't ipv6");

        return;
    };

    let source = ipv6_packet.get_source();
    let destination = ipv6_packet.get_destination();

    match ipv6_packet.get_next_header() {
        IpNextHeaderProtocols::Udp => {
            let Some(mut udp_packet) = udp::MutableUdpPacket::new(ipv6_packet.payload_mut()) else {
                return;
            };

            udp_packet.set_checksum(udp::ipv6_checksum(
                &udp_packet.to_immutable(),
                &source,
                &destination,
            ))
        }
        IpNextHeaderProtocols::Tcp => {
            let Some(mut tcp_packet) = tcp::MutableTcpPacket::new(ipv6_packet.payload_mut()) else {
                return;
            };

            tcp_packet.set_checksum(tcp::ipv6_checksum(
                &tcp_packet.to_immutable(),
                &source,
                &destination,
            ))
        }
        _ => {}
    }
}
