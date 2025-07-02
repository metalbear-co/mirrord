use std::{fmt, net::IpAddr};

use bincode::{Decode, Encode};

use crate::Payload;

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct NetworkConfiguration {
    pub ip: IpAddr,
    pub net_mask: IpAddr,
    pub gateway: IpAddr,
}

#[derive(Encode, Decode, PartialEq, Eq, Clone)]
pub enum ClientVpn {
    GetNetworkConfiguration,
    OpenSocket,
    Packet(Payload),
}

impl fmt::Debug for ClientVpn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClientVpn::GetNetworkConfiguration => f.debug_tuple("GetNetworkConfiguration").finish(),
            ClientVpn::OpenSocket => f.debug_tuple("OpenSocket").finish(),
            ClientVpn::Packet(packet) => f
                .debug_tuple("Packet")
                .field(&format!("{} bytes", packet.len()))
                .finish(),
        }
    }
}

/// Messages related to Tcp handler from server.
#[derive(Encode, Decode, PartialEq, Eq, Clone)]
pub enum ServerVpn {
    NetworkConfiguration(NetworkConfiguration),
    Packet(Payload),
}

impl fmt::Debug for ServerVpn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ServerVpn::NetworkConfiguration(config) => f
                .debug_tuple("NetworkConfiguration")
                .field(&config)
                .finish(),
            ServerVpn::Packet(packet) => f
                .debug_tuple("Packet")
                .field(&format!("{} bytes", packet.len()))
                .finish(),
        }
    }
}
