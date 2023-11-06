//! Utilities for handling multiple network protocol stacks within one
//! [`OutgoingProxy`](super::OutgoingProxy).

use std::{
    env, io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::PathBuf,
};

use mirrord_intproxy_protocol::NetProtocol;
use mirrord_protocol::{
    outgoing::{
        tcp::LayerTcpOutgoing, udp::LayerUdpOutgoing, LayerClose, LayerConnect, LayerWrite,
        SocketAddress, UnixAddr,
    },
    ClientMessage, ConnectionId,
};
use rand::{distributions::Alphanumeric, Rng};
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket, UnixListener, UnixStream},
};

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
                bytes,
            })),
            Self::Stream => ClientMessage::TcpOutgoing(LayerTcpOutgoing::Write(LayerWrite {
                connection_id,
                bytes,
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
            SocketAddress::Unix(..) => match self {
                Self::Stream => {
                    let path = PreparedSocket::generate_uds_path().await?;
                    PreparedSocket::UnixListener(UnixListener::bind(path)?)
                }
                Self::Datagrams => {
                    tracing::error!("layer requested intercepting outgoing datagrams over unix socket, this is not supported");
                    panic!("layer requested outgoing datagrams over unix sockets");
                }
            },
        };

        Ok(socket)
    }
}

/// A socket prepared to accept an intercepted connection.
pub enum PreparedSocket {
    /// There is no real listening/accepting here, see [`NetProtocol::Datagrams`] for more info.
    UdpSocket(UdpSocket),
    TcpListener(TcpListener),
    UnixListener(UnixListener),
}

impl PreparedSocket {
    /// For unix listeners, relative to the temp dir.
    const UNIX_STREAMS_DIRNAME: &'static str = "mirrord-unix-sockets";

    async fn generate_uds_path() -> io::Result<PathBuf> {
        let tmp_dir = env::temp_dir().join(Self::UNIX_STREAMS_DIRNAME);
        if !tmp_dir.exists() {
            fs::create_dir_all(&tmp_dir).await?;
        }

        let random_string: String = rand::thread_rng()
            .sample_iter(Alphanumeric)
            .take(16)
            .map(char::from)
            .collect();

        Ok(tmp_dir.join(random_string))
    }

    /// Returns the address of this socket.
    pub fn local_address(&self) -> io::Result<SocketAddress> {
        let address = match self {
            Self::TcpListener(listener) => listener.local_addr()?.into(),
            Self::UdpSocket(socket) => socket.local_addr()?.into(),
            Self::UnixListener(listener) => {
                let addr = listener.local_addr()?;
                let pathname = addr.as_pathname().unwrap().to_path_buf();
                SocketAddress::Unix(UnixAddr::Pathname(pathname))
            }
        };

        Ok(address)
    }

    /// Accepts one connection on this socket and returns a new socket for sending and receiving
    /// data.
    pub async fn accept(self) -> io::Result<ConnectedSocket> {
        let (inner, is_really_connected, buf_size) = match self {
            Self::TcpListener(listener) => {
                let (stream, _) = listener.accept().await?;
                (InnerConnectedSocket::TcpStream(stream), true, 1024)
            }
            Self::UdpSocket(socket) => (InnerConnectedSocket::UdpSocket(socket), false, 1500),
            Self::UnixListener(listener) => {
                let (stream, _) = listener.accept().await?;
                (InnerConnectedSocket::UnixStream(stream), true, 1024)
            }
        };

        Ok(ConnectedSocket {
            inner,
            is_really_connected,
            buffer: vec![0; buf_size],
        })
    }
}

enum InnerConnectedSocket {
    UdpSocket(UdpSocket),
    TcpStream(TcpStream),
    UnixStream(UnixStream),
}

/// A socket for intercepted connection with the layer.
pub struct ConnectedSocket {
    inner: InnerConnectedSocket,
    /// Meaningful only when `inner` is [`InnerConnectedSocket::UdpSocket`].
    is_really_connected: bool,
    buffer: Vec<u8>,
}

impl ConnectedSocket {
    /// Sends all given data to the layer.
    pub async fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
        match &mut self.inner {
            InnerConnectedSocket::UdpSocket(socket) => {
                let bytes_sent = socket.send(bytes).await?;

                if bytes_sent != bytes.len() {
                    Err(io::Error::new(
                        io::ErrorKind::Other,
                        "failed to send all bytes",
                    ))?;
                }

                Ok(())
            }
            InnerConnectedSocket::TcpStream(stream) => {
                stream.write_all(bytes).await.map_err(Into::into)
            }
            InnerConnectedSocket::UnixStream(stream) => {
                stream.write_all(bytes).await.map_err(Into::into)
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

                let received = socket.recv(&mut self.buffer).await?;

                let bytes = self.buffer.get(..received).unwrap().to_vec();

                Ok(bytes)
            }
            InnerConnectedSocket::TcpStream(stream) => {
                let received = stream.read(&mut self.buffer).await?;
                Ok(self.buffer.get(..received).unwrap().to_vec())
            }
            InnerConnectedSocket::UnixStream(stream) => {
                let received = stream.read(&mut self.buffer).await?;
                Ok(self.buffer.get(..received).unwrap().to_vec())
            }
        }
    }
}
