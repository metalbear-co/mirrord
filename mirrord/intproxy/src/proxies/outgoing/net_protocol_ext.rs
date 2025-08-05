//! Utilities for handling multiple network protocol stacks within one
//! [`OutgoingProxy`](super::OutgoingProxy).

#[cfg(not(windows))]
use std::{env, path::PathBuf};
use std::{
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
};

#[cfg(not(windows))]
use ::tokio::fs;
use ::tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
};
use bytes::BytesMut;
use mirrord_intproxy_protocol::NetProtocol;
#[cfg(not(windows))]
use mirrord_protocol::outgoing::UnixAddr;
use mirrord_protocol::{
    ClientMessage, ConnectionId,
    outgoing::{
        LayerClose, LayerConnect, LayerWrite, SocketAddress, tcp::LayerTcpOutgoing,
        udp::LayerUdpOutgoing,
    },
};
#[cfg(not(target_os = "windows"))]
use mirrord_protocol::outgoing::UnixAddr;
#[cfg(not(target_os = "windows"))]
use rand::distr::{Alphanumeric, SampleString};
#[cfg(target_os = "windows")]
mod tokio {
    pub mod net {
        pub struct UnixStream {}
        pub struct UnixListener {}
    }
}
use tokio::net::{UnixListener, UnixStream};

/// Trait for [`NetProtocol`] that handles differences in [`mirrord_protocol::outgoing`] between
/// network protocols. Allows to unify logic.
pub trait NetProtocolExt: Sized {
    /// Creates a [`LayerWrite`] message and wraps it into the common [`ClientMessage`] type.
    /// The enum path used here depends on this protocol.
    fn wrap_agent_write(self, connection_id: ConnectionId, bytes: Vec<u8>) -> ClientMessage;

    /// Creates a [`LayerClose`] message and wraps it into the common [`ClientMessage`] type.
    /// The enum path used here depends on this protocol.
    fn wrap_agent_close(self, connection_id: ConnectionId) -> ClientMessage;

    /// Creates a [`LayerConnect`] message and wraps it into the common [`ClientMessage`] type.
    /// The enum path used here depends on this protocol.
    fn wrap_agent_connect(self, remote_address: SocketAddress) -> ClientMessage;

    /// Opens a new socket for intercepting a connection to the given remote address.
    async fn prepare_socket(self, for_remote_address: SocketAddress) -> io::Result<PreparedSocket>;
}

impl NetProtocolExt for NetProtocol {
    fn wrap_agent_write(self, connection_id: ConnectionId, bytes: Vec<u8>) -> ClientMessage {
        match self {
            Self::Datagrams => ClientMessage::UdpOutgoing(LayerUdpOutgoing::Write(LayerWrite {
                connection_id,
                bytes: bytes.into(),
            })),
            Self::Stream => ClientMessage::TcpOutgoing(LayerTcpOutgoing::Write(LayerWrite {
                connection_id,
                bytes: bytes.into(),
            })),
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
        }
    }

    fn wrap_agent_connect(self, remote_address: SocketAddress) -> ClientMessage {
        match self {
            Self::Datagrams => {
                ClientMessage::UdpOutgoing(LayerUdpOutgoing::Connect(LayerConnect {
                    remote_address,
                }))
            }
            Self::Stream => ClientMessage::TcpOutgoing(LayerTcpOutgoing::Connect(LayerConnect {
                remote_address,
            })),
        }
    }

    async fn prepare_socket(self, for_remote_address: SocketAddress) -> io::Result<PreparedSocket> {
        let socket = match for_remote_address {
            SocketAddress::Ip(addr) => {
                let ip_addr = match addr.ip() {
                    IpAddr::V4(..) => IpAddr::V4(Ipv4Addr::LOCALHOST),
                    IpAddr::V6(..) => IpAddr::V6(Ipv6Addr::LOCALHOST),
                };
                let bind_at = SocketAddr::new(ip_addr, 0);

                match self {
                    Self::Datagrams => PreparedSocket::UdpSocket(UdpSocket::bind(bind_at).await?),
                    Self::Stream => PreparedSocket::TcpListener(TcpListener::bind(bind_at).await?),
                }
            }
            #[cfg(not(target_os = "windows"))]
            SocketAddress::Unix(..) => match self {
                Self::Stream => {
                    let path = PreparedSocket::generate_uds_path().await?;
                    PreparedSocket::UnixListener(UnixListener::bind(path)?)
                }
                Self::Datagrams => {
                    tracing::error!(
                        "layer requested intercepting outgoing datagrams over unix socket, this is not supported"
                    );
                    panic!("layer requested outgoing datagrams over unix sockets");
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
pub enum PreparedSocket {
    /// There is no real listening/accepting here, see [`NetProtocol::Datagrams`] for more info.
    UdpSocket(UdpSocket),
    TcpListener(TcpListener),
    #[allow(dead_code)]
    UnixListener(UnixListener),
}

impl PreparedSocket {
    /// For unix listeners, relative to the temp dir.
    #[cfg(not(windows))]
    const UNIX_STREAMS_DIRNAME: &'static str = "mirrord-unix-sockets";

    #[cfg(not(windows))]
    async fn generate_uds_path() -> io::Result<PathBuf> {
        let tmp_dir = env::temp_dir().join(Self::UNIX_STREAMS_DIRNAME);
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
            #[cfg(not(target_os = "windows"))]
            Self::UnixListener(listener) => {
                let addr = listener.local_addr()?;
                let pathname = addr.as_pathname().unwrap().to_path_buf();
                SocketAddress::Unix(UnixAddr::Pathname(pathname))
            }
            #[cfg(target_os = "windows")]
            Self::UnixListener(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "Unsupported UnixListener",
                ));
            }
        };

        Ok(address)
    }

    /// Accepts one connection on this socket and returns a new socket for sending and receiving
    /// data.
    pub async fn accept(self) -> io::Result<ConnectedSocket> {
        let (inner, is_really_connected) = match self {
            Self::TcpListener(listener) => {
                let (stream, _) = listener.accept().await?;
                (InnerConnectedSocket::TcpStream(stream), true)
            }
            Self::UdpSocket(socket) => (InnerConnectedSocket::UdpSocket(socket), false),
            #[cfg(not(target_os = "windows"))]
            Self::UnixListener(listener) => {
                let (stream, _) = listener.accept().await?;
                (InnerConnectedSocket::UnixStream(stream), true)
            }
            #[cfg(target_os = "windows")]
            Self::UnixListener(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "unsupported UnixListener",
                ));
            }
        };

        Ok(ConnectedSocket {
            inner,
            is_really_connected,
            buffer: BytesMut::with_capacity(64 * 1024),
        })
    }
}

enum InnerConnectedSocket {
    UdpSocket(UdpSocket),
    TcpStream(TcpStream),
    #[allow(dead_code)]
    UnixStream(UnixStream),
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
            #[cfg(not(target_os = "windows"))]
            InnerConnectedSocket::UnixStream(stream) => stream.write_all(bytes).await,
            #[cfg(target_os = "windows")]
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "unsupported InnerConnectedSocket",
            )),
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
            #[cfg(not(target_os = "windows"))]
            InnerConnectedSocket::UnixStream(stream) => {
                stream.read_buf(&mut self.buffer).await?;
                let bytes = self.buffer.to_vec();
                self.buffer.clear();
                Ok(bytes)
            }
            #[cfg(target_os = "windows")]
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "unsupported InnerConnectedSocket",
            )),
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
            #[cfg(not(target_os = "windows"))]
            InnerConnectedSocket::UnixStream(stream) => stream.shutdown().await,
            InnerConnectedSocket::UdpSocket(..) => Ok(()),
            #[cfg(target_os = "windows")]
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "unsupported InnerConnectedSocket",
            )),
        }
    }
}
