use mirrord_protocol::Port;
use tokio::sync::{broadcast, mpsc::Sender, oneshot};

use super::TcpSessionIdentifier;
use crate::util::ClientId;

#[derive(Debug)]
pub enum SnifferCommandInner {
    NewClient(Sender<SniffedConnection>),
    Subscribe(
        // Number of port to subscribe.
        Port,
        // Channel to notify with `()` when the subscription is done.
        oneshot::Sender<()>,
    ),
    UnsubscribePort(Port),
}

#[derive(Debug)]
pub struct SnifferCommand {
    pub client_id: ClientId,
    pub command: SnifferCommandInner,
}

pub struct SniffedConnection {
    pub session_id: TcpSessionIdentifier,
    pub data: broadcast::Receiver<Vec<u8>>,
}
