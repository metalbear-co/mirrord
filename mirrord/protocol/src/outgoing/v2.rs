use bincode::{Decode, Encode};

use crate::{Payload, ResponseError, outgoing::SocketAddress, uid::Uid};

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
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone, Copy)]
pub enum OutgoingProtocol {
    Udp,
    Tcp,
}

/// Fatal failure of an outgoing connection, sent from the daemon to the client.
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct OutgoingError {
    /// ID of the connection.
    pub id: Uid,
    /// The error that failed the connection.
    pub error: ResponseError,
}
