use std::net::SocketAddr;

use bincode::{Decode, Encode};

pub mod connection_handoff;
pub mod error;

pub use connection_handoff::{
    CONNECTION_HANDOFF_SOCKET_ENV, ConnectionHandoff, ConnectionHandoffServer,
    handle_connection_handoff_connection,
};

/// Metadata sent alongside a transferred accepted socket fd on the connection handoff side channel.
#[derive(Encode, Decode, Debug, Eq, PartialEq, Hash, Clone)]
pub struct AcceptHandoffRequest {
    /// Stable identifier for the accepted socket origin record.
    pub accept_id: u64,
    /// Address of the listener that accepted the connection.
    pub listener_address: SocketAddr,
    /// Address of the accepted socket on the local host.
    pub local_address: SocketAddr,
    /// Address of connection peer.
    pub peer_address: SocketAddr,
}

/// Verdict sent over the connection handoff side channel.
#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteAcceptVerdict {
    Decline,
    Claim { placeholder_address: SocketAddr },
}

/// A response to the connection handoff request.
#[derive(Encode, Decode, Debug, Clone, PartialEq, Eq)]
pub struct AcceptHandoffResponse {
    /// Identifier of the accepted socket origin record.
    pub accept_id: u64,
    /// Final sidecar verdict for this accepted socket.
    pub verdict: RemoteAcceptVerdict,
    /// Address of the listener that accepted the connection.
    pub listener_address: SocketAddr,
    /// Address of the accepted socket on the local host.
    pub local_address: SocketAddr,
    /// Address of connection peer.
    pub peer_address: SocketAddr,
}
