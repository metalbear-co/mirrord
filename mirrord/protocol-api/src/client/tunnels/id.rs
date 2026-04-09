use bytes::Bytes;
use mirrord_protocol::{
    ClientMessage,
    outgoing::{LayerClose, LayerWrite, tcp::LayerTcpOutgoing, udp::LayerUdpOutgoing},
    tcp::{LayerTcp, LayerTcpSteal, TcpData},
};
use strum_macros::VariantArray;

/// Type of traffic tunnel.
///
/// Within each type, connection ids are tracked independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, VariantArray)]
pub enum TunnelType {
    OutgoingTcp,
    OutgoingUdp,
    IncomingSteal,
    IncomingMirror,
}

impl TunnelType {
    /// Returns whether connections of this type produce outbound traffic.
    ///
    /// Currently, only mirrored incoming connections do not.
    pub fn has_outbound(self) -> bool {
        match self {
            Self::IncomingMirror => false,
            Self::IncomingSteal | Self::OutgoingTcp | Self::OutgoingUdp => true,
        }
    }
}

/// Full ID of a tunneled connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TunnelId(
    /// Type of the tunnel.
    pub TunnelType,
    /// ID received from the [`mirrord_protocol`] server.
    pub u64,
);

impl TunnelId {
    /// Produces a [`ClientMessage`] to close this connection.
    pub fn close_message(self) -> ClientMessage {
        match self.0 {
            TunnelType::OutgoingTcp => {
                ClientMessage::TcpOutgoing(LayerTcpOutgoing::Close(LayerClose {
                    connection_id: self.1,
                }))
            }
            TunnelType::OutgoingUdp => {
                ClientMessage::UdpOutgoing(LayerUdpOutgoing::Close(LayerClose {
                    connection_id: self.1,
                }))
            }
            TunnelType::IncomingSteal => {
                ClientMessage::TcpSteal(LayerTcpSteal::ConnectionUnsubscribe(self.1))
            }
            TunnelType::IncomingMirror => {
                ClientMessage::Tcp(LayerTcp::ConnectionUnsubscribe(self.1))
            }
        }
    }

    /// Produces a [`ClientMessage`] to send outbound data through this connection.
    ///
    /// Returns [`None`] if this connection does not produce outbound data (see
    /// [`TunnelType::has_outbound`]).
    pub fn write_message(self, data: Bytes) -> Option<ClientMessage> {
        match self.0 {
            TunnelType::OutgoingTcp => Some(ClientMessage::TcpOutgoing(LayerTcpOutgoing::Write(
                LayerWrite {
                    connection_id: self.1,
                    bytes: data.into(),
                },
            ))),
            TunnelType::OutgoingUdp => Some(ClientMessage::UdpOutgoing(LayerUdpOutgoing::Write(
                LayerWrite {
                    connection_id: self.1,
                    bytes: data.into(),
                },
            ))),
            TunnelType::IncomingSteal => {
                Some(ClientMessage::TcpSteal(LayerTcpSteal::Data(TcpData {
                    connection_id: self.1,
                    bytes: data.into(),
                })))
            }
            TunnelType::IncomingMirror => None,
        }
    }
}
