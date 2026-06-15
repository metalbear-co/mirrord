use super::*;
use crate::RemoteResult;

/// Layer messages for the `SOCK_SEQPACKET` socket.
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum LayerSeqpacket {
    /// Write one packet to the remote address the agent is connected to.
    Write(LayerWrite),

    /// The layer closed the connection, this message syncs up the agent, closing it there as
    /// well.
    Close(LayerClose),

    /// User is interested in connecting via unix seqpacket to some remote address.
    ConnectV2(LayerConnectV2),
}

/// Daemon messages for the `SOCK_SEQPACKET` socket.
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum DaemonSeqpacket {
    /// Read one packet from the connection.
    Read(RemoteResult<DaemonRead>),

    /// Tell the layer that this connection has been closed.
    Close(ConnectionId),

    /// The agent attempted a connection, tracked back to the request with a [`Uid`].
    ConnectV2(DaemonConnectV2),
}
