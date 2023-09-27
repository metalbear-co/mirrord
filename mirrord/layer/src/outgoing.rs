use core::fmt;
use std::ops::{Deref, DerefMut};

use mirrord_protocol::{
    ConnectionId,
};
use socket2::SockAddr;

use crate::common::ResponseChannel;

/// Wrapper type for the (layer) socket address that intercepts the user's socket messages.
///
/// (user-app) user_app_address <--> layer_address (layer) <--> agent <--> remote-peer
#[derive(Debug)]
pub(crate) struct RemoteConnection {
    /// The socket that is held by mirrord.
    pub(crate) layer_address: SockAddr,
    /// The socket that is held by the user application.
    pub(crate) user_app_address: SockAddr,
}

#[derive(Debug)]
pub(crate) struct Connect {
    pub(crate) remote_address: SockAddr,
    pub(crate) channel_tx: ResponseChannel<RemoteConnection>,
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
