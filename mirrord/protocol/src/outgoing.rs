use core::fmt;
use std::{
    fmt::{Display, Formatter},
    io,
    io::ErrorKind,
    net::SocketAddr as StdIpSocketAddr,
    path::PathBuf,
};

use bincode::{Decode, Encode};
use socket2::SockAddr as OsSockAddr;

use crate::{
    ConnectionId,
    Payload,
    // outgoing::UnixAddr::{Abstract, Pathname, Unnamed},
    SerializationError,
};

pub mod tcp;
pub mod udp;

/// A serializable socket address type that can represent IP addresses or addresses of unix sockets.
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum SocketAddress {
    Ip(StdIpSocketAddr),
    Unix(UnixAddr),
}

impl SocketAddress {
    pub fn get_port(&self) -> Option<u16> {
        match self {
            SocketAddress::Ip(address) => Some(address.port()),
            SocketAddress::Unix(_) => None,
        }
    }
}

/// A unix socket address type with rust member types (not libc stuff).
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum UnixAddr {
    Pathname(PathBuf),
    Abstract(Vec<u8>),
    Unnamed,
}

impl From<StdIpSocketAddr> for SocketAddress {
    fn from(addr: StdIpSocketAddr) -> Self {
        SocketAddress::Ip(addr)
    }
}

#[cfg(not(windows))]
impl TryFrom<UnixAddr> for OsSockAddr {
    type Error = io::Error;

    fn try_from(addr: UnixAddr) -> Result<Self, Self::Error> {
        match addr {
            Pathname(path) => OsSockAddr::unix(path),
            // We could use a `socket2::SockAddr::from_abstract_name` but it does not have it yet.
            Abstract(bytes) => OsSockAddr::unix(String::from_utf8(bytes).map_err(|_| {
                io::Error::new(
                    ErrorKind::Other,
                    "Unprintable abstract addresses not supported.",
                )
            })?),
            UnixAddr::Unnamed => OsSockAddr::unix(""),
        }
    }
}

impl TryFrom<SocketAddress> for StdIpSocketAddr {
    type Error = io::Error;

    fn try_from(addr: SocketAddress) -> Result<Self, Self::Error> {
        #[allow(irrefutable_let_patterns)]
        if let SocketAddress::Ip(socket_addr) = addr {
            Ok(socket_addr)
        } else {
            Err(io::Error::new(ErrorKind::Other, "Not an IP address"))
        }
    }
}

impl TryFrom<OsSockAddr> for SocketAddress {
    type Error = SerializationError;

    fn try_from(addr: OsSockAddr) -> Result<Self, Self::Error> {
        let res = addr.as_socket().map(SocketAddress::Ip);
        #[cfg(not(windows))]
        {
            res = res
                .or_else(|| {
                    addr.as_pathname()
                        .map(|path| SocketAddress::Unix(Pathname(path.to_owned())))
                })
                .or_else(|| {
                    addr.as_abstract_namespace()
                        .map(|slice| SocketAddress::Unix(Abstract(slice.to_vec())))
                })
                .or_else(|| addr.is_unnamed().then_some(SocketAddress::Unix(Unnamed)));
        }
        res.ok_or(SerializationError::SocketAddress)
    }
}

impl TryFrom<SocketAddress> for OsSockAddr {
    type Error = io::Error;

    fn try_from(addr: SocketAddress) -> Result<Self, Self::Error> {
        match addr {
            SocketAddress::Ip(socket_addr) => Ok(socket_addr.into()),
            #[cfg(not(windows))]
            SocketAddress::Unix(unix_addr) => unix_addr.try_into(),
            #[cfg(windows)]
            _ => Err(
                Self::Error::new(ErrorKind::InvalidInput, SerializationError::SocketAddress),
            ),
        }
    }
}

/// For error messages, e.g. `RemoteError::ConnectTimeOut`.
impl Display for SocketAddress {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let addr = match self {
            SocketAddress::Ip(ip_address) => ip_address.to_string(),
            #[cfg(not(windows))]
            SocketAddress::Unix(Pathname(path)) => path.to_string_lossy().to_string(),
            #[cfg(not(windows))]
            SocketAddress::Unix(Abstract(name)) => {
                String::from_utf8_lossy(name.as_ref()).to_string()
            }
            #[cfg(not(windows))]
            SocketAddress::Unix(Unnamed) => "<UNNAMED-UNIX-ADDRESS>".to_string(),
            #[cfg(windows)]
            _ => "<UNSUPPORTED-UNIX-ADDRESS>".to_string(),
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
    pub bytes: Payload,
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
    pub bytes: Payload,
}

impl fmt::Debug for DaemonRead {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DaemonRead")
            .field("connection_id", &self.connection_id)
            .field("bytes (length)", &self.bytes.len())
            .finish()
    }
}
