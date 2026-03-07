use core::fmt;
use std::{
    fmt::{Display, Formatter},
    io, net,
    path::PathBuf,
    sync::LazyLock,
};

use bincode::{Decode, Encode};
use semver::VersionReq;

use crate::{ConnectionId, Payload, RemoteResult, SerializationError, uid::Uid};

pub mod tcp;
pub mod udp;

/// Minimal mirrord-protocol version that allows for [`LayerConnectV2`] and [`DaemonConnectV2`].
pub static OUTGOING_CONNECT_V2: LazyLock<VersionReq> =
    LazyLock::new(|| ">=1.22.0".parse().expect("Bad Identifier"));

/// A serializable socket address type that can represent IP addresses or addresses of unix sockets.
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum SocketAddress {
    Ip(net::SocketAddr),
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

impl From<net::SocketAddr> for SocketAddress {
    fn from(addr: net::SocketAddr) -> Self {
        Self::Ip(addr)
    }
}

#[cfg(unix)]
impl TryFrom<UnixAddr> for socket2::SockAddr {
    type Error = io::Error;

    fn try_from(addr: UnixAddr) -> Result<Self, Self::Error> {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

        match addr {
            UnixAddr::Pathname(path) => socket2::SockAddr::unix(path),
            UnixAddr::Abstract(mut bytes) => {
                // Abstract names are "paths" that start with a NUL byte.
                bytes.insert(0, 0);
                socket2::SockAddr::unix(OsStr::from_bytes(&bytes))
            }
            UnixAddr::Unnamed => socket2::SockAddr::unix(""),
        }
    }
}

impl TryFrom<SocketAddress> for net::SocketAddr {
    type Error = io::Error;

    fn try_from(addr: SocketAddress) -> Result<Self, Self::Error> {
        if let SocketAddress::Ip(socket_addr) = addr {
            Ok(socket_addr)
        } else {
            Err(io::Error::other("Not an IP address"))
        }
    }
}

impl TryFrom<socket2::SockAddr> for SocketAddress {
    type Error = SerializationError;

    #[cfg(windows)]
    fn try_from(addr: socket2::SockAddr) -> Result<Self, Self::Error> {
        addr.as_socket()
            .map(Self::Ip)
            .ok_or(SerializationError::SocketAddress)
    }

    #[cfg(unix)]
    fn try_from(addr: socket2::SockAddr) -> Result<Self, Self::Error> {
        addr.as_socket()
            .map(Self::Ip)
            .or_else(|| {
                addr.as_pathname()
                    .map(|path| Self::Unix(UnixAddr::Pathname(path.to_owned())))
            })
            .or_else(|| {
                addr.as_abstract_namespace()
                    .map(|slice| Self::Unix(UnixAddr::Abstract(slice.to_vec())))
            })
            .or_else(|| addr.is_unnamed().then_some(Self::Unix(UnixAddr::Unnamed)))
            .ok_or(SerializationError::SocketAddress)
    }
}

impl TryFrom<SocketAddress> for socket2::SockAddr {
    type Error = io::Error;

    fn try_from(addr: SocketAddress) -> Result<Self, Self::Error> {
        match addr {
            SocketAddress::Ip(socket_addr) => Ok(socket_addr.into()),
            #[cfg(unix)]
            SocketAddress::Unix(unix_addr) => unix_addr.try_into(),
            #[cfg(windows)]
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
        match self {
            SocketAddress::Ip(ip_address) => ip_address.fmt(f),
            #[cfg(unix)]
            SocketAddress::Unix(UnixAddr::Pathname(path)) => {
                write!(f, "<UNIX-PATH> {}", path.display())
            }
            #[cfg(unix)]
            SocketAddress::Unix(UnixAddr::Abstract(name)) => {
                write!(f, "<UNIX-ABSTRACT> {}", String::from_utf8_lossy(name))
            }
            #[cfg(unix)]
            SocketAddress::Unix(UnixAddr::Unnamed) => f.write_str("<UNIX-UNNAMED>"),
            #[cfg(windows)]
            _ => f.write_str("<UNSUPPORTED-UNIX-ADDRESS>"),
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

#[derive(Debug, Encode, Decode, PartialEq, Eq, Clone)]
pub struct LayerConnectV2 {
    /// Unique ID of this request.
    pub uid: Uid,
    /// Remote address to connect to.
    pub remote_address: SocketAddress,
}

#[derive(Debug, Encode, Decode, PartialEq, Eq, Clone)]
pub struct DaemonConnectV2 {
    /// Copied from the original [`LayerConnectV2`] request.
    pub uid: Uid,
    /// Result of the connection attempt.
    pub connect: RemoteResult<DaemonConnect>,
}

#[cfg(all(test, target_os = "linux"))]
mod test {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

    use socket2::SockAddr;

    use crate::outgoing::{SocketAddress, UnixAddr};

    #[test]
    fn abstract_unix_conversion() {
        let name = b"\0very-very-abstract-name";

        let addr = SockAddr::unix(OsStr::from_bytes(name)).unwrap();
        assert_eq!(addr.as_abstract_namespace().unwrap(), &name[1..]);

        let protocol_addr = SocketAddress::try_from(addr).unwrap();
        assert_eq!(
            protocol_addr,
            SocketAddress::Unix(UnixAddr::Abstract(name.get(1..).unwrap().to_vec())),
        );

        let converted = SockAddr::try_from(protocol_addr).unwrap();
        assert_eq!(converted.as_abstract_namespace().unwrap(), &name[1..]);
    }

    #[test]
    fn unnamed_unix_conversion() {
        let addr = SockAddr::unix("").unwrap();
        assert!(addr.is_unnamed());

        let protocol_addr = SocketAddress::try_from(addr).unwrap();
        assert_eq!(protocol_addr, SocketAddress::Unix(UnixAddr::Unnamed));

        let converted = SockAddr::try_from(protocol_addr).unwrap();
        assert!(converted.is_unnamed());
    }
}
