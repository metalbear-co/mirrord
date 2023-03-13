use std::{
    io,
    io::Error,
    os::{
        linux::net::SocketAddrExt,
        unix::net::{SocketAddr as StdUnixSocketAddr, UnixStream as StdUnixStream},
    },
    pin::Pin,
    task::{Context, Poll},
};

use mirrord_protocol::outgoing::{
    SocketAddress,
    UnixAddr::{Abstract, Pathname},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpStream, UnixStream},
};

pub enum SocketStream {
    Ip(TcpStream),
    Unix(UnixStream),
}

impl From<TcpStream> for SocketStream {
    fn from(tcp_stream: TcpStream) -> Self {
        Self::Ip(tcp_stream)
    }
}

impl From<UnixStream> for SocketStream {
    fn from(unix_stream: UnixStream) -> Self {
        Self::Unix(unix_stream)
    }
}

impl SocketStream {
    pub fn local_addr(&self) -> io::Result<SocketAddress> {
        Ok(match self {
            SocketStream::Ip(tcp_stream) => SocketAddress::Ip(tcp_stream.local_addr()?),
            SocketStream::Unix(unix_stream) => {
                // TODO: unwrap!
                // TODO: If tokio's UnixStream's SocketAddress does not support abstract addresses
                //      should we support only pathname? Or should we not use tokio?
                SocketAddress::Unix(Pathname(
                    unix_stream.local_addr()?.as_pathname().unwrap().to_owned(),
                ))
            }
        })
    }

    pub async fn connect(addr: SocketAddress) -> io::Result<Self> {
        Ok(match addr {
            SocketAddress::Ip(addr) => Self::from(TcpStream::connect(addr).await?),
            SocketAddress::Unix(Pathname(path)) => Self::from(UnixStream::connect(&path).await?),
            SocketAddress::Unix(Abstract(bytes)) => {
                // Tokio's UnixStream does not support connecting to an abstract address so we first
                // create an std UnixStream, then we convert it to a tokio UnixStream.
                Self::from(UnixStream::from_std(StdUnixStream::connect_addr(
                    &<StdUnixSocketAddr as SocketAddrExt>::from_abstract_name(bytes)?,
                )?)?)
            }
        })
    }
}

impl AsyncRead for SocketStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            // TODO: review, help please - is this correct? ↓
            SocketStream::Ip(tcp_stream) => Pin::new(tcp_stream).poll_read(cx, buf),
            SocketStream::Unix(unix_stream) => Pin::new(unix_stream).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for SocketStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, Error>> {
        match self.get_mut() {
            SocketStream::Ip(tcp_stream) => Pin::new(tcp_stream).poll_write(cx, buf),
            SocketStream::Unix(unix_stream) => Pin::new(unix_stream).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        match self.get_mut() {
            SocketStream::Ip(tcp_stream) => Pin::new(tcp_stream).poll_flush(cx),
            SocketStream::Unix(unix_stream) => Pin::new(unix_stream).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Error>> {
        match self.get_mut() {
            SocketStream::Ip(tcp_stream) => Pin::new(tcp_stream).poll_shutdown(cx),
            SocketStream::Unix(unix_stream) => Pin::new(unix_stream).poll_shutdown(cx),
        }
    }
}
