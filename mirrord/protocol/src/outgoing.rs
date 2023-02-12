use core::fmt;
use std::net::SocketAddr;

use bincode::{Decode, Encode};

use crate::ConnectionId;

pub mod tcp;
pub mod udp;

/// `user` wants to connect to `remote_address`.
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct LayerConnect {
    pub remote_address: SocketAddr,
}

/// `user` wants to write `bytes` to remote host identified by `connection_id`.
#[derive(Encode, Decode, PartialEq, Eq, Clone)]
pub struct LayerWrite {
    pub connection_id: ConnectionId,
    pub bytes: Vec<u8>,
}

impl fmt::Debug for LayerWrite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LayerWrite")
            .field("connection_id", &self.connection_id)
            .field("bytes (length)", &self.bytes.len())
            .finish()
    }
}

/// `layer` interceptor socket closed or failed.
#[derive(Debug, Encode, Decode, PartialEq, Eq, Clone)]
pub struct LayerClose {
    pub connection_id: ConnectionId,
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct DaemonConnect {
    pub connection_id: ConnectionId,
    pub remote_address: SocketAddr,
    pub local_address: SocketAddr,
}

#[derive(Encode, Decode, PartialEq, Eq, Clone)]
pub struct DaemonRead {
    pub connection_id: ConnectionId,
    pub bytes: Vec<u8>,
}

impl fmt::Debug for DaemonRead {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DaemonRead")
            .field("connection_id", &self.connection_id)
            .field("bytes (length)", &self.bytes.len())
            .finish()
    }
}
