use core::fmt;
use std::{
    net::SocketAddr,
    ops::{Deref, DerefMut},
};

use mirrord_protocol::{
    outgoing::{DaemonConnect, DaemonRead, LayerClose, LayerConnect, LayerWrite},
    ConnectionId,
};

use crate::common::ResponseChannel;

pub(crate) mod tcp;
pub(crate) mod udp;

/// Wrapper type for the (layer) socket address that intercepts the user's socket messages.
#[derive(Debug)]
pub(crate) struct RemoteConnectResult {
    pub(crate) mirror_address: SocketAddr,
    pub(crate) local_address: SocketAddr,
}

#[derive(Debug)]
pub(crate) struct Connect {
    pub(crate) remote_address: SocketAddr,
    pub(crate) channel_tx: ResponseChannel<RemoteConnectResult>,
}

pub(crate) struct Write {
    pub(crate) connection_id: ConnectionId,
    pub(crate) bytes: Vec<u8>,
}

impl fmt::Debug for Write {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Write")
            .field("id", &self.connection_id)
            .field("bytes (length)", &self.bytes.len())
            .finish()
    }
}

/// Wrapper type around `tokio::Sender`, used to send messages from the `agent` to our interceptor
/// socket, where they'll be written back to the user's socket.
///
/// (agent) -> (layer) -> (user)
#[derive(Debug)]
pub(crate) struct ConnectionMirror(tokio::sync::mpsc::Sender<Vec<u8>>);

impl Deref for ConnectionMirror {
    type Target = tokio::sync::mpsc::Sender<Vec<u8>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for ConnectionMirror {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
