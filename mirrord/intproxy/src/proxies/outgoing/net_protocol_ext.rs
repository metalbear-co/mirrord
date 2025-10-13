//! Utilities for handling multiple network protocol stacks within one
//! [`OutgoingProxy`](super::OutgoingProxy).

#[cfg(not(target_os = "windows"))]
use std::{env, path::PathBuf};
use std::{
    io,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
};

use bytes::BytesMut;
#[cfg(not(target_os = "windows"))]
use mirrord_protocol::outgoing::UnixAddr;
use mirrord_protocol::outgoing::{SocketAddress, v2};
#[cfg(not(target_os = "windows"))]
use rand::distr::{Alphanumeric, SampleString};
#[cfg(not(target_os = "windows"))]
use tokio::{
    fs,
    net::{UnixListener, UnixStream},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpSocket, TcpStream, UdpSocket},
};

/// Opens a new socket for intercepting a connection to the given remote address.
pub async fn prepare_socket(
    remote_address: &SocketAddress,
    proto: v2::OutgoingProtocol,
    non_blocking_tcp: bool,
) -> io::Result<PreparedSocket> {
    match (remote_address, proto) {
        #[cfg(not(target_os = "windows"))]
        (SocketAddress::Ip(addr), v2::OutgoingProtocol::Stream) if non_blocking_tcp => {
            let socket = if addr.is_ipv4() {
                let socket = TcpSocket::new_v4()?;
                socket.bind(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0))?;
                socket
            } else {
                let socket = TcpSocket::new_v6()?;
                socket.bind(SocketAddr::new(Ipv6Addr::LOCALHOST.into(), 0))?;
                socket
            };
            let backlog = if cfg!(target_os = "linux") { 0 } else { 1 };
            let listener = socket.listen(backlog)?;
            let dummy = TcpStream::connect(listener.local_addr()?).await?;
            Ok(PreparedSocket::TcpListener {
                listener,
                dummy: Some(dummy),
            })
        }

        (SocketAddress::Ip(addr), v2::OutgoingProtocol::Stream) => {
            let bind_to = if addr.is_ipv4() {
                Ipv4Addr::LOCALHOST.into()
            } else {
                Ipv6Addr::LOCALHOST.into()
            };
            let bind_to = SocketAddr::new(bind_to, 0);
            let listener = TcpListener::bind(bind_to).await?;
            Ok(PreparedSocket::TcpListener {
                listener,
                dummy: None,
            })
        }

        (SocketAddress::Ip(addr), v2::OutgoingProtocol::Datagrams) => {
            let bind_to = if addr.is_ipv4() {
                Ipv4Addr::LOCALHOST.into()
            } else {
                Ipv6Addr::LOCALHOST.into()
            };
            let bind_to = SocketAddr::new(bind_to, 0);
            let socket = UdpSocket::bind(bind_to).await.unwrap();
            Ok(PreparedSocket::UdpSocket(socket))
        }

        #[cfg(not(target_os = "windows"))]
        (SocketAddress::Unix(..), v2::OutgoingProtocol::Stream) => {
            let path = PreparedSocket::generate_uds_path().await?;
            let listener = UnixListener::bind(path)?;
            Ok(PreparedSocket::UnixListener(listener))
        }

        #[cfg(not(target_os = "windows"))]
        (SocketAddress::Unix(..), v2::OutgoingProtocol::Datagrams) => {
            tracing::error!(
                "layer requested intercepting outgoing datagrams over unix socket, \
                this is not supported"
            );
            panic!("layer requested outgoing datagrams over unix sockets");
        }

        #[cfg(target_os = "windows")]
        _ => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "unsupported SocketAddress",
            ));
        }
    }
}

/// A socket prepared to accept an intercepted connection.
#[derive(Debug)]
pub enum PreparedSocket {
    /// There is no real listening/accepting here, see [`v2::OutgoingProtocol::Datagrams`] for more
    /// info.
    UdpSocket(UdpSocket),
    TcpListener {
        listener: TcpListener,
        dummy: Option<TcpStream>,
    },
    #[cfg(not(target_os = "windows"))]
    UnixListener(UnixListener),
}

impl PreparedSocket {
    /// For unix listeners, relative to the temp dir.
    #[cfg(not(target_os = "windows"))]
    const UNIX_STREAMS_DIRNAME: &'static str = "mirrord-unix-sockets";

    #[cfg(not(target_os = "windows"))]
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
            Self::TcpListener { listener, .. } => listener.local_addr()?.into(),
            Self::UdpSocket(socket) => socket.local_addr()?.into(),
            #[cfg(not(target_os = "windows"))]
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
        let (inner, is_really_connected) = match self {
            Self::TcpListener {
                listener,
                dummy: None,
            } => {
                let (stream, _) = listener.accept().await?;
                (InnerConnectedSocket::TcpStream(stream), true)
            }
            Self::TcpListener {
                listener,
                dummy: Some(dummy),
            } => {
                let dummy_address = dummy.local_addr()?;
                std::mem::drop(dummy);
                loop {
                    let (stream, addr) = listener.accept().await?;
                    if addr != dummy_address {
                        break (InnerConnectedSocket::TcpStream(stream), true);
                    }
                }
            }
            Self::UdpSocket(socket) => (InnerConnectedSocket::UdpSocket(socket), false),
            #[cfg(not(target_os = "windows"))]
            Self::UnixListener(listener) => {
                let (stream, _) = listener.accept().await?;
                (InnerConnectedSocket::UnixStream(stream), true)
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
    #[cfg(not(target_os = "windows"))]
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
        }
    }
}

#[cfg(test)]
mod test {
    use std::{net::SocketAddr, time::Duration};

    use mirrord_protocol::outgoing::{SocketAddress, v2};
    use tokio::net::TcpStream;

    use crate::proxies::outgoing::net_protocol_ext::{InnerConnectedSocket, prepare_socket};

    /// Verifies making a connection to a TCP [`PreparedSocket`](super::PreparedSocket)
    /// cannot finish before [`PreparedSocket::accept`](super::PreparedSocket::accept) is called.
    #[cfg_attr(target_os = "windows", ignore)]
    #[tokio::test]
    async fn tcp_connect_blocked() {
        let socket = prepare_socket(
            &SocketAddress::Ip("127.0.0.1:4444".parse().unwrap()),
            v2::OutgoingProtocol::Stream,
            true,
        )
        .await
        .unwrap();
        let addr: SocketAddr = socket.local_address().unwrap().try_into().unwrap();

        let connect_task = tokio::spawn(TcpStream::connect(addr));

        tokio::time::sleep(Duration::from_millis(500)).await;
        if connect_task.is_finished() {
            panic!("connect task finished early: {:?}", connect_task.await);
        }

        let connected = socket.accept().await.unwrap();
        let InnerConnectedSocket::TcpStream(stream_server) = connected.inner else {
            unreachable!();
        };
        let stream_client = connect_task.await.unwrap().unwrap();
        assert_eq!(
            stream_server.peer_addr().unwrap(),
            stream_client.local_addr().unwrap()
        );
        assert_eq!(
            stream_client.peer_addr().unwrap(),
            stream_server.local_addr().unwrap()
        );
    }
}
