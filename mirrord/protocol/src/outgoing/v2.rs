use std::fmt;

use bincode::{Decode, Encode};

use crate::{
    ClientMessage, ConnectionId, DaemonMessage, Payload, RemoteResult, ResponseError,
    outgoing::{
        DaemonConnect, DaemonRead, LayerClose, LayerConnect, LayerWrite, SocketAddress,
        tcp::{DaemonTcpOutgoing, LayerTcpOutgoing},
        udp::{DaemonUdpOutgoing, LayerUdpOutgoing},
    },
    uid::Uid,
};

/// Client messages for the outgoing traffic feature.
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum ClientOutgoing {
    /// Request to open a new remote connection.
    Connect(OutgoingConnectRequest),
    /// Data to be sent through a remote connection.
    Data(OutgoingData),
    /// Request to close a remote connection.
    Close(OutgoingClose),
}

/// Daemon messages for the outgoing traffic feature.
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum DaemonOutgoing {
    /// Confirmation sent after opening a new remote connection.
    Connect(OutgoingConnectResponse),
    /// Data received from a remote connection.
    Data(OutgoingData),
    /// Notification about a remote connection being closed.
    Close(OutgoingClose),
    /// Notification about a fatal failure related to a remote connection.
    Error(OutgoingError),
}

/// Client request to open a new remote connection.
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct OutgoingConnectRequest {
    /// ID for the new connection.
    pub id: Uid,
    /// Address of the peer.
    pub address: SocketAddress,
    /// Transport layer protocol to use for the connection.
    pub protocol: OutgoingProtocol,
}

/// Daemon's positive response to client's [`OutgoingConnectRequest`].
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct OutgoingConnectResponse {
    /// ID of the connection, copied from the request.
    pub id: Uid,
    /// Local address of the agent's socket.
    pub agent_local_address: SocketAddress,
    /// Peer address of the agent's socket.
    pub agent_peer_address: SocketAddress,
}

/// Data received from one end of an outgoing connection.
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct OutgoingData {
    /// ID of the connection.
    pub id: Uid,
    /// Received data.
    ///
    /// 0-sized data means write shutdown from the peer.
    pub data: Payload,
}

/// Close of an outgoing connection.
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone, Copy)]
pub struct OutgoingClose {
    /// ID of the connection.
    pub id: Uid,
}

/// Transport layer protocol of an outgoing connection.
///
/// Provides convenience methods for wrapping v1 outgoing messages into [`DaemonMessage`]s.
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone, Copy, Hash)]
pub enum OutgoingProtocol {
    Udp,
    Tcp,
}

impl OutgoingProtocol {
    pub fn v1_daemon_close(self, connection_id: ConnectionId) -> DaemonMessage {
        match self {
            Self::Tcp => DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Close(connection_id)),
            Self::Udp => DaemonMessage::UdpOutgoing(DaemonUdpOutgoing::Close(connection_id)),
        }
    }

    pub fn v1_daemon_read(self, read: DaemonRead) -> DaemonMessage {
        match self {
            Self::Tcp => DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Read(Ok(read))),
            Self::Udp => DaemonMessage::UdpOutgoing(DaemonUdpOutgoing::Read(Ok(read))),
        }
    }

    pub fn v1_daemon_connect(self, connect: RemoteResult<DaemonConnect>) -> DaemonMessage {
        match self {
            Self::Tcp => DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Connect(connect)),
            Self::Udp => DaemonMessage::UdpOutgoing(DaemonUdpOutgoing::Connect(connect)),
        }
    }

    pub fn v1_layer_close(self, connection_id: ConnectionId) -> ClientMessage {
        match self {
            Self::Tcp => {
                ClientMessage::TcpOutgoing(LayerTcpOutgoing::Close(LayerClose { connection_id }))
            }
            Self::Udp => {
                ClientMessage::UdpOutgoing(LayerUdpOutgoing::Close(LayerClose { connection_id }))
            }
        }
    }

    pub fn v1_layer_write(self, write: LayerWrite) -> ClientMessage {
        match self {
            Self::Tcp => ClientMessage::TcpOutgoing(LayerTcpOutgoing::Write(write)),
            Self::Udp => ClientMessage::UdpOutgoing(LayerUdpOutgoing::Write(write)),
        }
    }

    pub fn v1_layer_connect(self, remote_address: SocketAddress) -> ClientMessage {
        match self {
            Self::Tcp => ClientMessage::TcpOutgoing(LayerTcpOutgoing::Connect(LayerConnect {
                remote_address,
            })),
            Self::Udp => ClientMessage::UdpOutgoing(LayerUdpOutgoing::Connect(LayerConnect {
                remote_address,
            })),
        }
    }
}

impl fmt::Display for OutgoingProtocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let as_str = match self {
            Self::Tcp => "tcp",
            Self::Udp => "udp",
        };
        f.write_str(as_str)
    }
}

/// Fatal failure of an outgoing connection, sent from the daemon to the client.
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct OutgoingError {
    /// ID of the connection.
    pub id: Uid,
    /// The error that failed the connection.
    pub error: ResponseError,
}
