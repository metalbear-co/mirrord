use std::net::SocketAddr;

use bincode::{Decode, Encode};

pub mod connection_handoff;
pub mod error;

pub use connection_handoff::{
    handle_connection_handoff_connection, ConnectionHandoff, ConnectionHandoffServer,
    RemoteLayerSubscriptionsView, CONNECTION_HANDOFF_SOCKET_ENV,
};

/// Metadata sent alongside a transferred accepted socket fd on the connection handoff side channel.
#[derive(Encode, Decode, Debug, Eq, PartialEq, Hash, Clone)]
pub struct ConnectionHandoffRequest {
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
pub enum ConnectionHandoffVerdict {
    /// The remote-layer sidecar does not want to take this accepted connection, so the layer
    /// keeps the original accepted socket and falls back to local handling.
    Rejected,
    /// The remote-layer sidecar accepted the connection and created a local placeholder listener
    /// at this address. The layer should connect a replacement placeholder socket here and return
    /// that fd to the application instead of the original accepted socket fd.
    Accepted { placeholder_address: SocketAddr },
}

/// A response to the connection handoff request.
#[derive(Encode, Decode, Debug, Clone, PartialEq, Eq)]
pub struct ConnectionHandoffResponse {
    /// Identifier of the accepted socket origin record.
    pub accept_id: u64,
    /// Final sidecar verdict for this accepted socket.
    pub verdict: ConnectionHandoffVerdict,
    /// Address of the listener that accepted the connection.
    pub listener_address: SocketAddr,
    /// Address of the accepted socket on the local host.
    pub local_address: SocketAddr,
    /// Address of connection peer.
    pub peer_address: SocketAddr,
}
