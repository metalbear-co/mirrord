use core::fmt;
use std::{
    fmt::{Display, Formatter},
    io,
    io::ErrorKind,
    net::SocketAddr,
    path::PathBuf,
};

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

impl TryFrom<UnixAddr> for SockAddr {
    type Error = io::Error;

    fn try_from(addr: UnixAddr) -> Result<Self, Self::Error> {
        match addr {
            Pathname(path) => SockAddr::unix(path),
            // TODO
            Abstract(bytes) => SockAddr::unix(String::from_utf8(bytes).map_err(|_| {
                io::Error::new(
                    ErrorKind::Other,
                    "Not supporting unprintable abstract addresses.",
                )
            })?),
        }
    }
}

impl TryFrom<SocketAddress> for SocketAddr {
    type Error = io::Error;

    fn try_from(addr: SocketAddress) -> Result<Self, Self::Error> {
        if let SocketAddress::Ip(socket_addr) = addr {
            Ok(socket_addr)
        } else {
            Err(io::Error::new(
                ErrorKind::Other,
                "Not supporting unprintable abstract addresses.",
            ))
        }
    }
}

impl SocketAddress {
    pub fn is_ip(&self) -> bool {
        matches!(self, Self::Ip(_))
    }

    pub fn is_unix(&self) -> bool {
        matches!(self, Self::Unix(_))
    }
}

impl TryFrom<SockAddr> for SocketAddress {
    type Error = SerializationError;

    fn try_from(addr: SockAddr) -> Result<Self, Self::Error> {
        addr.as_socket()
            .map(SocketAddress::Ip)
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
            SocketAddress::Unix(unix_addr) => unix_addr.try_into(),
        }
    }
}

/// For error messages, e.g. [`RemoteError::ConnectTimeOut`].
impl Display for SocketAddress {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let addr = match self {
            SocketAddress::Ip(ip_address) => ip_address.to_string(),
            SocketAddress::Unix(Pathname(path)) => path.to_string_lossy().to_string(),
            SocketAddress::Unix(Abstract(name)) => String::from_utf8_lossy(name).to_string(),
        };
        write!(f, "{addr}")
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
