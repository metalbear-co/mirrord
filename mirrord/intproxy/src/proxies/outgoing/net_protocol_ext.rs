//! Utilities for handling multiple network protocol stacks within one
//! [`OutgoingProxy`](super::OutgoingProxy).

#[cfg(unix)]
use std::{env, path::PathBuf};
use std::{
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    time::Duration,
};
#[cfg(unix)]
use std::{os::unix::fs::PermissionsExt, path::Path};

#[cfg(unix)]
use ::tokio::fs;
use ::tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
};
use bytes::{Bytes, BytesMut};
#[cfg(unix)]
use mirrord_config::internal_proxy::MIRRORD_INTPROXY_CONTAINER_MODE_ENV;
use mirrord_intproxy_protocol::NetProtocol;
#[cfg(unix)]
use mirrord_protocol::outgoing::UnixAddr;
use mirrord_protocol::{
    ClientMessage, ConnectionId,
    outgoing::{
        LayerClose, LayerConnect, LayerConnectV2, LayerWrite, SocketAddress,
        seqpacket::LayerSeqpacket, tcp::LayerTcpOutgoing, udp::LayerUdpOutgoing,
    },
    uid::Uid,
};
#[cfg(unix)]
use rand::distr::{Alphanumeric, SampleString};
use socket2::SockRef;
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};
#[cfg(all(unix, not(target_os = "macos")))]
use tokio_seqpacket::{UnixSeqpacket, UnixSeqpacketListener};

#[cfg(unix)]
use super::UNIX_STREAMS_DIRNAME;

#[cfg(unix)]
const CONTAINER_UNIX_SOCKET_MODE: u32 = 0o666;

#[cfg(unix)]
fn intproxy_container_mode() -> bool {
    env::var(MIRRORD_INTPROXY_CONTAINER_MODE_ENV)
        .ok()
        .and_then(|value| value.parse::<bool>().ok())
        .unwrap_or_default()
}

/// Makes a Unix bridge listener connectable by an application container running as another UID.
///
/// Container mode shares a per-session anonymous volume with the application, while host-mode
/// sockets can have stronger local-user isolation and therefore keep their default permissions.
#[cfg(unix)]
fn make_unix_listener_connectable(path: &Path, container_mode: bool) -> io::Result<()> {
    if container_mode {
        std::fs::set_permissions(
            path,
            std::fs::Permissions::from_mode(CONTAINER_UNIX_SOCKET_MODE),
        )?;
    }

    Ok(())
}

/// Trait for [`NetProtocol`] that handles differences in [`mirrord_protocol::outgoing`] between
/// network protocols. Allows to unify logic.
pub trait NetProtocolExt: Sized {
    /// Creates a [`LayerWrite`] message and wraps it into the common [`ClientMessage`] type.
    /// The enum path used here depends on this protocol.
    fn wrap_agent_write(self, connection_id: ConnectionId, bytes: Bytes) -> ClientMessage;

    /// Creates a [`LayerClose`] message and wraps it into the common [`ClientMessage`] type.
    /// The enum path used here depends on this protocol.
    fn wrap_agent_close(self, connection_id: ConnectionId) -> ClientMessage;

    /// Creates a [`LayerConnect`] message and wraps it into the common [`ClientMessage`] type.
    /// The enum path used here depends on this protocol.
    fn wrap_agent_connect(self, remote_address: SocketAddress, uid: Option<Uid>) -> ClientMessage;

    /// Opens a new socket for intercepting a connection to the given remote address.
    async fn prepare_socket(self, for_remote_address: SocketAddress) -> io::Result<PreparedSocket>;
}

impl NetProtocolExt for NetProtocol {
    fn wrap_agent_write(self, connection_id: ConnectionId, bytes: Bytes) -> ClientMessage {
        match self {
            Self::Datagrams => ClientMessage::UdpOutgoing(LayerUdpOutgoing::Write(LayerWrite {
                connection_id,
                bytes: bytes.into(),
            })),
            Self::Stream => ClientMessage::TcpOutgoing(LayerTcpOutgoing::Write(LayerWrite {
                connection_id,
                bytes: bytes.into(),
            })),
            Self::Seqpacket => {
                ClientMessage::SeqpacketOutgoing(LayerSeqpacket::Write(LayerWrite {
                    connection_id,
                    bytes: bytes.into(),
                }))
            }
        }
    }

    fn wrap_agent_close(self, connection_id: ConnectionId) -> ClientMessage {
        match self {
            Self::Datagrams => {
                ClientMessage::UdpOutgoing(LayerUdpOutgoing::Close(LayerClose { connection_id }))
            }
            Self::Stream => {
                ClientMessage::TcpOutgoing(LayerTcpOutgoing::Close(LayerClose { connection_id }))
            }
            Self::Seqpacket => {
                ClientMessage::SeqpacketOutgoing(LayerSeqpacket::Close(LayerClose {
                    connection_id,
                }))
            }
        }
    }

    fn wrap_agent_connect(self, remote_address: SocketAddress, uid: Option<Uid>) -> ClientMessage {
        match (self, uid) {
            (Self::Datagrams, None) => {
                ClientMessage::UdpOutgoing(LayerUdpOutgoing::Connect(LayerConnect {
                    remote_address,
                }))
            }
            (Self::Datagrams, Some(uid)) => {
                ClientMessage::UdpOutgoing(LayerUdpOutgoing::ConnectV2(LayerConnectV2 {
                    uid,
                    remote_address,
                }))
            }
            (Self::Stream, None) => {
                ClientMessage::TcpOutgoing(LayerTcpOutgoing::Connect(LayerConnect {
                    remote_address,
                }))
            }
            (Self::Stream, Some(uid)) => {
                ClientMessage::TcpOutgoing(LayerTcpOutgoing::ConnectV2(LayerConnectV2 {
                    uid,
                    remote_address,
                }))
            }
            (Self::Seqpacket, None) => {
                unreachable!("unix seqpacket outgoing connections require ConnectV2")
            }
            (Self::Seqpacket, Some(uid)) => {
                ClientMessage::SeqpacketOutgoing(LayerSeqpacket::ConnectV2(LayerConnectV2 {
                    uid,
                    remote_address,
                }))
            }
        }
    }

    async fn prepare_socket(self, for_remote_address: SocketAddress) -> io::Result<PreparedSocket> {
        let socket = match for_remote_address {
            SocketAddress::Ip(addr) => {
                let ip_addr = match addr.ip() {
                    IpAddr::V4(..) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                    IpAddr::V6(..) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
                };
                let bind_at = SocketAddr::new(ip_addr, 0);

                match self {
                    Self::Datagrams => PreparedSocket::UdpSocket(UdpSocket::bind(bind_at).await?),
                    Self::Stream => PreparedSocket::TcpListener(TcpListener::bind(bind_at).await?),
                    Self::Seqpacket => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "seqpacket outgoing supports only unix socket addresses",
                        ));
                    }
                }
            }
            #[cfg(unix)]
            SocketAddress::Unix(..) => match self {
                Self::Stream => {
                    let path = PreparedSocket::generate_uds_path().await?;
                    let listener = UnixListener::bind(&path)?;
                    make_unix_listener_connectable(&path, intproxy_container_mode())?;
                    PreparedSocket::UnixListener(listener)
                }
                Self::Datagrams => {
                    tracing::error!(
                        "layer requested intercepting outgoing datagrams over unix socket, this is not supported"
                    );
                    panic!("layer requested outgoing datagrams over unix sockets");
                }
                Self::Seqpacket => {
                    #[cfg(all(unix, not(target_os = "macos")))]
                    {
                        let path = PreparedSocket::generate_uds_path().await?;
                        let listener = UnixSeqpacketListener::bind(&path)?;
                        make_unix_listener_connectable(&path, intproxy_container_mode())?;
                        PreparedSocket::UnixSeqpacketListener(listener)
                    }

                    #[cfg(any(not(unix), target_os = "macos"))]
                    {
                        return Err(io::Error::new(
                            io::ErrorKind::Unsupported,
                            "seqpacket outgoing is not supported on this platform",
                        ));
                    }
                }
            },
            #[cfg(target_os = "windows")]
            _ => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "unsupported SocketAddress",
                ));
            }
        };

        Ok(socket)
    }
}

/// A socket prepared to accept an intercepted connection.
#[derive(Debug)]
pub enum PreparedSocket {
    /// There is no real listening/accepting here, see [`NetProtocol::Datagrams`] for more info.
    UdpSocket(UdpSocket),
    TcpListener(TcpListener),
    #[cfg(unix)]
    UnixListener(UnixListener),
    #[cfg(all(unix, not(target_os = "macos")))]
    UnixSeqpacketListener(UnixSeqpacketListener),
}

impl PreparedSocket {
    #[cfg(unix)]
    async fn generate_uds_path() -> io::Result<PathBuf> {
        let tmp_dir = env::temp_dir().join(UNIX_STREAMS_DIRNAME);
        if !tmp_dir.exists() {
            fs::create_dir_all(&tmp_dir).await?;
        }

        let random_string: String = Alphanumeric.sample_string(&mut rand::rng(), 16);
        Ok(tmp_dir.join(random_string))
    }

    /// Returns the address of this socket.
    pub fn local_address(&self) -> io::Result<SocketAddress> {
        let address = match self {
            Self::TcpListener(listener) => listener.local_addr()?.into(),
            Self::UdpSocket(socket) => socket.local_addr()?.into(),
            #[cfg(unix)]
            Self::UnixListener(listener) => {
                let addr = listener.local_addr()?;
                let pathname = addr.as_pathname().unwrap().to_path_buf();
                SocketAddress::Unix(UnixAddr::Pathname(pathname))
            }
            #[cfg(all(unix, not(target_os = "macos")))]
            Self::UnixSeqpacketListener(listener) => {
                SocketAddress::Unix(UnixAddr::Pathname(listener.local_addr()?))
            }
        };

        Ok(address)
    }

    /// Accepts one connection on this socket and returns a new socket
    /// for sending and receiving data.
    pub async fn accept(self) -> io::Result<ConnectedSocket> {
        let (inner, is_really_connected) = match self {
            Self::TcpListener(listener) => {
                let (stream, _) = listener.accept().await?;
                // This socket relays the application's outgoing traffic to the agent, so
                // buffering small writes here only adds latency to the connection. Failing to
                // set it costs latency, not correctness, so it must not fail the connection.
                if let Err(error) = stream.set_nodelay(true) {
                    tracing::warn!(%error, "Failed to set TCP_NODELAY on an intercepted outgoing connection");
                }
                (InnerConnectedSocket::TcpStream(stream), true)
            }
            Self::UdpSocket(socket) => (InnerConnectedSocket::UdpSocket(socket), false),
            #[cfg(unix)]
            Self::UnixListener(listener) => {
                let (stream, _) = listener.accept().await?;
                (InnerConnectedSocket::UnixStream(stream), true)
            }
            #[cfg(all(unix, not(target_os = "macos")))]
            Self::UnixSeqpacketListener(mut listener) => {
                let stream = listener.accept().await?;
                (InnerConnectedSocket::UnixSeqpacket(stream), true)
            }
        };

        Ok(ConnectedSocket {
            inner,
            is_really_connected,
            buffer: BytesMut::with_capacity(READ_BUFFER_BYTES),
        })
    }
}

/// Size of the buffer used for a single read from an intercepted connection.
///
/// Caps how many bytes a single [`ConnectedSocket::receive`] can return.
pub(super) const READ_BUFFER_BYTES: usize = 64 * 1024;

enum InnerConnectedSocket {
    UdpSocket(UdpSocket),
    TcpStream(TcpStream),
    #[cfg(unix)]
    UnixStream(UnixStream),
    #[cfg(all(unix, not(target_os = "macos")))]
    UnixSeqpacket(UnixSeqpacket),
}

/// A socket for intercepted connection with the layer.
pub struct ConnectedSocket {
    inner: InnerConnectedSocket,
    /// Meaningful only when `inner` is [`InnerConnectedSocket::UdpSocket`].
    is_really_connected: bool,
    buffer: BytesMut,
}

impl ConnectedSocket {
    /// Sends all given data to the layer.
    pub async fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
        match &mut self.inner {
            InnerConnectedSocket::UdpSocket(socket) => {
                let bytes_sent = socket.send(bytes).await?;

                if bytes_sent != bytes.len() {
                    Err(io::Error::other("failed to send all bytes"))?;
                }

                Ok(())
            }
            InnerConnectedSocket::TcpStream(stream) => stream.write_all(bytes).await,
            #[cfg(unix)]
            InnerConnectedSocket::UnixStream(stream) => stream.write_all(bytes).await,
            #[cfg(all(unix, not(target_os = "macos")))]
            InnerConnectedSocket::UnixSeqpacket(stream) => {
                let bytes_sent = stream.send(bytes).await?;

                if bytes_sent != bytes.len() {
                    Err(io::Error::other("failed to send all bytes"))?;
                }

                Ok(())
            }
        }
    }

    /// Receives some data from the layer.
    pub async fn receive(&mut self) -> io::Result<Vec<u8>> {
        match &mut self.inner {
            InnerConnectedSocket::UdpSocket(socket) => {
                if !self.is_really_connected {
                    let peer = socket.peek_sender().await?;
                    socket.connect(peer).await?;
                    self.is_really_connected = true;
                }

                socket.recv_buf(&mut self.buffer).await?;
                let bytes = self.buffer.to_vec();
                self.buffer.clear();
                Ok(bytes)
            }
            InnerConnectedSocket::TcpStream(stream) => {
                stream.read_buf(&mut self.buffer).await?;
                let bytes = self.buffer.to_vec();
                self.buffer.clear();
                Ok(bytes)
            }
            #[cfg(unix)]
            InnerConnectedSocket::UnixStream(stream) => {
                stream.read_buf(&mut self.buffer).await?;
                let bytes = self.buffer.to_vec();
                self.buffer.clear();
                Ok(bytes)
            }
            #[cfg(all(unix, not(target_os = "macos")))]
            InnerConnectedSocket::UnixSeqpacket(stream) => {
                self.buffer.resize(self.buffer.capacity(), 0);
                let bytes_read = stream.recv(&mut self.buffer).await?;
                self.buffer.truncate(bytes_read);
                let bytes = self.buffer.to_vec();
                self.buffer.clear();
                Ok(bytes)
            }
        }
    }

    /// Shuts the connection down. See [`AsyncWriteExt::shutdown`].
    ///
    /// # Note
    ///
    /// This is a no-op for UDP sockets.
    pub async fn shutdown(&mut self) -> io::Result<()> {
        match &mut self.inner {
            InnerConnectedSocket::TcpStream(stream) => stream.shutdown().await,
            #[cfg(unix)]
            InnerConnectedSocket::UnixStream(stream) => stream.shutdown().await,
            #[cfg(all(unix, not(target_os = "macos")))]
            InnerConnectedSocket::UnixSeqpacket(stream) => {
                stream.shutdown(std::net::Shutdown::Both)
            }
            InnerConnectedSocket::UdpSocket(..) => Ok(()),
        }
    }

    /// Makes dropping this socket abortive when the platform supports it.
    ///
    /// For TCP sockets, `SO_LINGER` with a zero timeout causes the close to send RST instead of a
    /// graceful FIN, which makes the peer observe a connection reset.
    pub fn reset(&mut self) -> io::Result<()> {
        match &mut self.inner {
            InnerConnectedSocket::TcpStream(stream) => {
                SockRef::from(&*stream).set_linger(Some(Duration::ZERO))
            }
            #[cfg(unix)]
            InnerConnectedSocket::UnixStream(..) => Ok(()),
            #[cfg(all(unix, not(target_os = "macos")))]
            InnerConnectedSocket::UnixSeqpacket(..) => Ok(()),
            InnerConnectedSocket::UdpSocket(..) => Ok(()),
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::{ffi::OsString, ops::Not, os::unix::fs::PermissionsExt, sync::Mutex};

    use tempfile::tempdir;

    use super::{
        CONTAINER_UNIX_SOCKET_MODE, MIRRORD_INTPROXY_CONTAINER_MODE_ENV, intproxy_container_mode,
        make_unix_listener_connectable,
    };

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvVarRestore(Option<OsString>);

    impl Drop for EnvVarRestore {
        fn drop(&mut self) {
            // SAFETY: ENV_LOCK serializes the test's environment mutations, and this restores
            // the original value before the lock is released.
            unsafe {
                match self.0.take() {
                    Some(value) => std::env::set_var(MIRRORD_INTPROXY_CONTAINER_MODE_ENV, value),
                    None => std::env::remove_var(MIRRORD_INTPROXY_CONTAINER_MODE_ENV),
                }
            }
        }
    }

    #[test]
    fn container_mode_requires_an_explicit_true_value() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        let _restore = EnvVarRestore(std::env::var_os(MIRRORD_INTPROXY_CONTAINER_MODE_ENV));

        // SAFETY: ENV_LOCK serializes the test's environment mutations, and EnvVarRestore
        // restores the original value before the lock is released.
        unsafe {
            std::env::remove_var(MIRRORD_INTPROXY_CONTAINER_MODE_ENV);
        }
        let absent = intproxy_container_mode();

        // SAFETY: see above.
        unsafe {
            std::env::set_var(MIRRORD_INTPROXY_CONTAINER_MODE_ENV, "invalid");
        }
        let invalid = intproxy_container_mode();

        // SAFETY: see above.
        unsafe {
            std::env::set_var(MIRRORD_INTPROXY_CONTAINER_MODE_ENV, "false");
        }
        let false_value = intproxy_container_mode();

        // SAFETY: see above.
        unsafe {
            std::env::set_var(MIRRORD_INTPROXY_CONTAINER_MODE_ENV, "true");
        }
        let true_value = intproxy_container_mode();

        assert!(absent.not());
        assert!(invalid.not());
        assert!(false_value.not());
        assert!(true_value);
    }

    #[test]
    fn listener_permissions_change_only_in_container_mode() {
        let directory = tempdir().expect("create temporary directory");
        let socket_path = directory.path().join("listener.sock");
        let _listener =
            std::os::unix::net::UnixListener::bind(&socket_path).expect("bind Unix listener");

        std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600))
            .expect("set socket permissions");
        make_unix_listener_connectable(&socket_path, false).expect("preserve socket permissions");
        let host_mode = std::fs::metadata(&socket_path)
            .expect("read host-mode socket metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(host_mode, 0o600);

        make_unix_listener_connectable(&socket_path, true).expect("relax socket permissions");
        let container_mode = std::fs::metadata(&socket_path)
            .expect("read container-mode socket metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(container_mode, CONTAINER_UNIX_SOCKET_MODE);
    }
}
