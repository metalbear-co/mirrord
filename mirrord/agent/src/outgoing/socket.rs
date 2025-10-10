use std::{
    borrow::Cow,
    ffi::OsStr,
    io,
    mem::MaybeUninit,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    os::{
        linux::net::SocketAddrExt,
        unix::{ffi::OsStrExt, net::SocketAddr as StdUnixSocketAddr},
    },
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use futures::Stream;
use mirrord_protocol::{
    RemoteError, RemoteResult, ResponseError,
    outgoing::{SocketAddress, UnixAddr, v2},
};
use tokio::{
    io::{AsyncRead, AsyncWriteExt, ReadBuf},
    net::{TcpStream, UdpSocket, UnixStream, tcp, unix},
};

use crate::{
    metrics::{MetricGuard, TCP_OUTGOING_CONNECTION, UDP_OUTGOING_CONNECTION},
    util::path_resolver::InTargetPathResolver,
};

pub async fn connect(
    address: &SocketAddress,
    protocol: v2::OutgoingProtocol,
    path_resolver: Option<&InTargetPathResolver>,
) -> RemoteResult<(ReadHalf, WriteHalf)> {
    let (read_inner, write_inner) = match (address, protocol) {
        (SocketAddress::Ip(address), v2::OutgoingProtocol::Tcp) => {
            let stream = TcpStream::connect(address).await?;
            let (read, write) = stream.into_split();
            (ReadHalfInner::Tcp(read), WriteHalfInner::Tcp(write))
        }

        (SocketAddress::Ip(address), v2::OutgoingProtocol::Udp) => {
            let bind_address = if address.is_ipv4() {
                Ipv4Addr::UNSPECIFIED.into()
            } else {
                Ipv6Addr::UNSPECIFIED.into()
            };
            let socket = UdpSocket::bind(SocketAddr::new(bind_address, 0)).await?;
            socket.connect(*address).await?;
            let socket = Arc::new(socket);
            (
                ReadHalfInner::Udp(socket.clone()),
                WriteHalfInner::Udp(socket),
            )
        }

        (SocketAddress::Unix(UnixAddr::Pathname(path)), v2::OutgoingProtocol::Tcp) => {
            let path = if let Some(resolver) = path_resolver.as_ref() {
                Cow::Owned(resolver.resolve(path)?)
            } else {
                Cow::Borrowed(path.as_path())
            };
            let stream = UnixStream::connect(path).await?;
            let (read, write) = stream.into_split();
            (ReadHalfInner::Unix(read), WriteHalfInner::Unix(write))
        }

        (SocketAddress::Unix(UnixAddr::Abstract(name)), v2::OutgoingProtocol::Tcp) => {
            let mut path = Vec::with_capacity(name.len() + 1);
            path.push(b'0');
            path.extend_from_slice(name);
            let stream = UnixStream::connect(OsStr::from_bytes(&path)).await?;
            let (read, write) = stream.into_split();
            (ReadHalfInner::Unix(read), WriteHalfInner::Unix(write))
        }

        (SocketAddress::Unix(UnixAddr::Unnamed), v2::OutgoingProtocol::Tcp) => {
            return Err(ResponseError::Remote(RemoteError::InvalidAddress(
                address.clone(),
            )));
        }

        (SocketAddress::Unix(..), v2::OutgoingProtocol::Udp) => {
            return Err(RemoteError::InvalidAddress(address.clone()).into());
        }
    };

    let metric = match protocol {
        v2::OutgoingProtocol::Tcp => &TCP_OUTGOING_CONNECTION,
        v2::OutgoingProtocol::Udp => &UDP_OUTGOING_CONNECTION,
    };
    let metric_guard = Arc::new(MetricGuard::new(metric));

    Ok((
        ReadHalf {
            _metric_guard: metric_guard.clone(),
            buffer: ReadHalf::make_buffer(),
            inner: read_inner,
        },
        WriteHalf {
            _metric_guard: metric_guard,
            inner: write_inner,
        },
    ))
}

pub struct ReadHalf {
    _metric_guard: Arc<MetricGuard>,
    buffer: Box<[MaybeUninit<u8>]>,
    inner: ReadHalfInner,
}

impl ReadHalf {
    const BUFFER_SIZE: usize = 1024 * 64;

    fn make_buffer() -> Box<[MaybeUninit<u8>]> {
        let mut vector = Vec::with_capacity(Self::BUFFER_SIZE);
        vector.resize_with(Self::BUFFER_SIZE, MaybeUninit::uninit);
        vector.into_boxed_slice()
    }
}

enum ReadHalfInner {
    Tcp(tcp::OwnedReadHalf),
    Udp(Arc<UdpSocket>),
    Unix(unix::OwnedReadHalf),
}

impl Stream for ReadHalf {
    type Item = io::Result<Vec<u8>>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let mut read_buf = ReadBuf::uninit(&mut this.buffer);
        let result = match &mut this.inner {
            ReadHalfInner::Tcp(stream) => Pin::new(stream).poll_read(cx, &mut read_buf),
            ReadHalfInner::Udp(socket) => Pin::new(socket).poll_recv(cx, &mut read_buf),
            ReadHalfInner::Unix(stream) => Pin::new(stream).poll_read(cx, &mut read_buf),
        };
        match result {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(())) => {
                if read_buf.filled().is_empty() {
                    Poll::Ready(None)
                } else {
                    Poll::Ready(Some(Ok(read_buf.filled().to_vec())))
                }
            }
            Poll::Ready(Err(error)) => Poll::Ready(Some(Err(error))),
        }
    }
}

pub struct WriteHalf {
    _metric_guard: Arc<MetricGuard>,
    inner: WriteHalfInner,
}

impl WriteHalf {
    pub async fn write(&mut self, data: &[u8]) -> io::Result<()> {
        match &mut self.inner {
            WriteHalfInner::Tcp(stream) => stream.write_all(data).await,
            WriteHalfInner::Udp(socket) => {
                socket.send(data).await?;
                Ok(())
            }
            WriteHalfInner::Unix(stream) => stream.write_all(data).await,
        }
    }

    pub fn local_address(&self) -> io::Result<SocketAddress> {
        match &self.inner {
            WriteHalfInner::Tcp(stream) => stream.local_addr().map(From::from),
            WriteHalfInner::Udp(socket) => socket.local_addr().map(From::from),
            WriteHalfInner::Unix(stream) => {
                let address = stream.local_addr()?;
                let address = StdUnixSocketAddr::from(address);
                if let Some(path) = address.as_pathname() {
                    Ok(SocketAddress::Unix(UnixAddr::Pathname(path.to_path_buf())))
                } else if let Some(name) = address.as_abstract_name() {
                    Ok(SocketAddress::Unix(UnixAddr::Abstract(name.to_vec())))
                } else {
                    Ok(SocketAddress::Unix(UnixAddr::Unnamed))
                }
            }
        }
    }

    pub fn peer_address(&self) -> io::Result<SocketAddress> {
        match &self.inner {
            WriteHalfInner::Tcp(stream) => stream.peer_addr().map(From::from),
            WriteHalfInner::Udp(socket) => socket.peer_addr().map(From::from),
            WriteHalfInner::Unix(stream) => {
                let address = stream.peer_addr()?;
                let address = StdUnixSocketAddr::from(address);
                if let Some(path) = address.as_pathname() {
                    Ok(SocketAddress::Unix(UnixAddr::Pathname(path.to_path_buf())))
                } else if let Some(name) = address.as_abstract_name() {
                    Ok(SocketAddress::Unix(UnixAddr::Abstract(name.to_vec())))
                } else {
                    Ok(SocketAddress::Unix(UnixAddr::Unnamed))
                }
            }
        }
    }
}

enum WriteHalfInner {
    Tcp(tcp::OwnedWriteHalf),
    Udp(Arc<UdpSocket>),
    Unix(unix::OwnedWriteHalf),
}
