use std::{
    env, io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::PathBuf,
};

use mirrord_protocol::{
    outgoing::{
        tcp::LayerTcpOutgoing, udp::LayerUdpOutgoing, LayerClose, LayerConnect, SocketAddress,
        UnixAddr,
    },
    ClientMessage, ConnectionId,
};
use rand::{distributions::Alphanumeric, Rng};
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket, UnixListener, UnixStream},
};

use crate::protocol::NetProtocol;

mod interceptor;
mod proxy;

pub use proxy::{OutgoingProxy, OutgoingProxyError, OutgoingProxyMessage};

impl NetProtocol {
    fn wrap_agent_close(&self, connection_id: ConnectionId) -> ClientMessage {
        match self {
            Self::Datagrams => {
                ClientMessage::TcpOutgoing(LayerTcpOutgoing::Close(LayerClose { connection_id }))
            }
            Self::Stream => {
                ClientMessage::UdpOutgoing(LayerUdpOutgoing::Close(LayerClose { connection_id }))
            }
        }
    }

    fn wrap_agent_connect(&self, remote_address: SocketAddress) -> ClientMessage {
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

enum PreparedSocket {
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

    fn local_address(&self) -> io::Result<SocketAddress> {
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

    async fn accept(self) -> io::Result<ConnectedSocket> {
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

struct ConnectedSocket {
    inner: InnerConnectedSocket,
    is_really_connected: bool,
    buffer: Vec<u8>,
}

impl ConnectedSocket {
    async fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
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

    async fn receive(&mut self) -> io::Result<Vec<u8>> {
        match &mut self.inner {
            InnerConnectedSocket::UdpSocket(socket) => {
                let (received, peer) = socket.recv_from(&mut self.buffer).await?;

                if !self.is_really_connected {
                    socket.connect(peer).await?;
                }

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
