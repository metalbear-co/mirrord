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

use mirrord_protocol::{
    outgoing::{
        SocketAddress,
        UnixAddr::{Abstract, Pathname, Unnamed},
    },
    RemoteError, RemoteResult, ResponseError,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpStream, UnixStream},
};

use crate::file::{get_root_path_from_optional_pid, resolve_path};

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
                let local_address = unix_stream.local_addr()?;
                SocketAddress::Unix(if local_address.is_unnamed() {
                    Unnamed
                } else {
                    // Unwrap: we probably don't connect from a local abstract address.
                    Pathname(local_address.as_pathname().unwrap().to_owned())
                })
            }
        })
    }

    pub async fn connect(addr: SocketAddress, pid: Option<u64>) -> RemoteResult<Self> {
        match addr {
            SocketAddress::Ip(addr) => Ok(Self::from(TcpStream::connect(addr).await?)),
            SocketAddress::Unix(Pathname(path)) => {
                // In order to connect to a unix socket on the target pod, instead of connecting to
                // /the/target/path we connect to /proc/<PID>/root/the/target/path.
                let root_path = get_root_path_from_optional_pid(pid);
                let final_path = resolve_path(path, root_path)?;

                Ok(Self::from(UnixStream::connect(&final_path).await?))
            }
            SocketAddress::Unix(Abstract(bytes)) => {
                // Tokio's UnixStream does not support connecting to an abstract address so we first
                // create an std UnixStream, then we convert it to a tokio UnixStream.
                Ok(Self::from(UnixStream::from_std(
                    StdUnixStream::connect_addr(
                        &<StdUnixSocketAddr as SocketAddrExt>::from_abstract_name(bytes)?,
                    )?,
                )?))
            }
            SocketAddress::Unix(Unnamed) => {
                // Cannot connect to unnamed socket by address.
                Err(ResponseError::Remote(RemoteError::InvalidAddress(addr)))
            }
        }
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
