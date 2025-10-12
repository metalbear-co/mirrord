use core::fmt;
use std::{
    fmt::{Display, Formatter},
    io,
    net::SocketAddr as StdIpSocketAddr,
    path::PathBuf,
    sync::LazyLock,
};

use bincode::{Decode, Encode};
use semver::VersionReq;
use socket2::SockAddr as OsSockAddr;

#[cfg(not(target_os = "windows"))]
use crate::outgoing::UnixAddr::{Abstract, Pathname, Unnamed};
use crate::{ConnectionId, Payload, SerializationError};

/// V1 of messages for the outgoing TPC traffic feature.
pub mod tcp;
/// V1 of messages for the outgoing UDP traffic feature.
pub mod udp;
/// V2 of messages for the outgoing traffic feature.
pub mod v2;

/// Minimal protocol version that allows for sending [`v2`] outgoing messages.
pub static OUTGOING_V2_VERSION: LazyLock<VersionReq> =
    LazyLock::new(|| ">=1.22.0".parse().expect("Bad Identifier"));

/// A serializable socket address type that can represent IP addresses or addresses of unix sockets.
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone, Hash)]
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
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone, Hash)]
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

#[cfg(not(target_os = "windows"))]
impl TryFrom<UnixAddr> for OsSockAddr {
    type Error = io::Error;

    fn try_from(addr: UnixAddr) -> Result<Self, Self::Error> {
        match addr {
            Pathname(path) => OsSockAddr::unix(path),
            // We could use a `socket2::SockAddr::from_abstract_name` but it does not have it yet.
            Abstract(bytes) => {
                OsSockAddr::unix(String::from_utf8(bytes).map_err(|_| {
                    io::Error::other("Unprintable abstract addresses not supported.")
                })?)
            }
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
            Err(io::Error::other("Not an IP address"))
        }
    }
}

impl TryFrom<OsSockAddr> for SocketAddress {
    type Error = SerializationError;

    #[cfg(target_os = "windows")]
    fn try_from(addr: OsSockAddr) -> Result<Self, Self::Error> {
        let res = addr.as_socket().map(SocketAddress::Ip);
        res.ok_or(SerializationError::SocketAddress)
    }

    #[cfg(not(target_os = "windows"))]
    fn try_from(addr: OsSockAddr) -> Result<Self, Self::Error> {
        let res = addr
            .as_socket()
            .map(SocketAddress::Ip)
            .or_else(|| {
                addr.as_pathname()
                    .map(|path| SocketAddress::Unix(Pathname(path.to_owned())))
            })
            .or_else(|| {
                addr.as_abstract_namespace()
                    .map(|slice| SocketAddress::Unix(Abstract(slice.to_vec())))
            })
            .or_else(|| addr.is_unnamed().then_some(SocketAddress::Unix(Unnamed)));
        res.ok_or(SerializationError::SocketAddress)
    }
}

impl TryFrom<SocketAddress> for OsSockAddr {
    type Error = io::Error;

    fn try_from(addr: SocketAddress) -> Result<Self, Self::Error> {
        match addr {
            SocketAddress::Ip(socket_addr) => Ok(socket_addr.into()),
            #[cfg(not(target_os = "windows"))]
            SocketAddress::Unix(unix_addr) => unix_addr.try_into(),
            #[cfg(target_os = "windows")]
            _ => Err(Self::Error::new(
                io::ErrorKind::InvalidInput,
                SerializationError::SocketAddress,
            )),
        }
    }
}

/// For error messages, e.g. `RemoteError::ConnectTimeOut`.
impl Display for SocketAddress {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let addr = match self {
            SocketAddress::Ip(ip_address) => ip_address.to_string(),
            #[cfg(not(target_os = "windows"))]
            SocketAddress::Unix(Pathname(path)) => path.to_string_lossy().to_string(),
            #[cfg(not(target_os = "windows"))]
            SocketAddress::Unix(Abstract(name)) => {
                String::from_utf8_lossy(name.as_ref()).to_string()
            }
            #[cfg(not(target_os = "windows"))]
            SocketAddress::Unix(Unnamed) => "<UNNAMED-UNIX-ADDRESS>".to_string(),
            #[cfg(target_os = "windows")]
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
