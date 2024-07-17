use mirrord_protocol::Port;
use tokio::sync::{broadcast, mpsc::Sender, oneshot};

use super::TcpSessionIdentifier;
use crate::util::ClientId;

/// Commmand for [`TcpConnectionSniffer`](super::TcpConnectionSniffer).
#[derive(Debug)]
pub(crate) enum SnifferCommandInner {
    /// New client wants to use the sniffer.
    NewClient(
        /// For notyfing the client about new incoming connections.
        Sender<SniffedConnection>,
    ),
    /// Client wants to start receiving connections incoming to a specific port.
    Subscribe(
        /// Number of port to subscribe.
        Port,
        /// Channel to notify with the same port number when the operation is done.
        oneshot::Sender<Port>,
    ),
    /// Client no longer wants to receive connections incoming to a specific port.
    UnsubscribePort(
        /// Number of port to unsubscribe.
        Port,
    ),
}

/// Client's command for [`TcpConnectionSniffer`](super::TcpConnectionSniffer).
#[derive(Debug)]
pub(crate) struct SnifferCommand {
    /// Id of the client.
    pub client_id: ClientId,
    /// Actual command.
    pub command: SnifferCommandInner,
}

/// New TCP connection picked up by [`TcpConnectionSniffer`](super::TcpConnectionSniffer).
pub(crate) struct SniffedConnection {
    /// Parameters of this connection's TCP session.
    /// Can be used to create [`NewTcpConnection`](mirrord_protocol::tcp::NewTcpConnection).
    pub session_id: TcpSessionIdentifier,
    /// For receiving data from this connection.
    pub data: broadcast::Receiver<Vec<u8>>,
}
