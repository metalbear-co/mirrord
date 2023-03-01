use core::fmt;
use std::{io, io::ErrorKind, net::SocketAddr, path::PathBuf};

use bincode::{Decode, Encode};
use socket2::SockAddr;

use crate::{
    outgoing::UnixAddr::{Abstract, Pathname},
    ConnectionId, SerializationError,
};

pub mod tcp;
pub mod udp;

/// A serializable socket address type that can represent IP addresses or addresses of unix sockets.
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum SocketAddress {
    Ip(SocketAddr),
    Unix(UnixAddr),
}

/// A unix socket address type that owns all of its data (does not contain references/slices).
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum UnixAddr {
    Pathname(PathBuf),
    Abstract(Vec<u8>),
}

impl TryFrom<SockAddr> for SocketAddress {
    type Error = SerializationError;

    fn try_from(addr: SockAddr) -> Result<Self, Self::Error> {
        addr.as_socket()
            .map(|ip_addr| SocketAddress::Ip(ip_addr))
            .or_else(|| {
                addr.as_pathname()
                    .map(|path| SocketAddress::Unix(Pathname(path.to_owned())))
            })
            .or_else(|| {
                addr.as_abstract_namespace()
                    .map(|slice| SocketAddress::Unix(Abstract(slice.to_vec())))
            })
            .ok_or(SerializationError::SocketAddress)
    }
}

impl TryFrom<SocketAddress> for SockAddr {
    type Error = io::Error;

    fn try_from(addr: SocketAddress) -> Result<Self, Self::Error> {
        match addr {
            SocketAddress::Ip(socket_addr) => Ok(socket_addr.into()),
            SocketAddress::Unix(Pathname(path)) => SockAddr::unix(path),
            SocketAddress::Unix(Abstract(bytes)) => {
                SockAddr::unix(String::from_utf8(bytes).map_err(|_| {
                    io::Error::new(
                        ErrorKind::Other,
                        "Not supporting unprintable abstract addresses.",
                    )
                })?)
            }
        }
    }
}

/// `user` wants to connect to `remote_address`.
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct LayerConnect {
    pub remote_address: SocketAddress,
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
    pub remote_address: SocketAddress,
    pub local_address: SocketAddress,
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
