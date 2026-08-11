use std::{
    ffi::OsStr,
    io,
    os::unix::ffi::OsStrExt,
    pin::Pin,
    sync::atomic::AtomicUsize,
    task::{Context, Poll},
};

use mirrord_protocol::{
    DaemonMessage,
    outgoing::{
        DaemonConnect, DaemonConnectV2, DaemonRead, SocketAddress, UnixAddr,
        tcp::{DaemonTcpOutgoing, LayerTcpOutgoing},
    },
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpStream, UnixStream, tcp, unix},
};
use tokio_util::io::ReaderStream;

use crate::{
    metrics,
    outgoing::{
        GenericInMessage, GenericOutMessage, OutgoingKind,
        router::{ConnectionKind, NewConnection},
    },
    util::{io::throttle::IoVecThrottledSink, path_resolver::InTargetPathResolver},
};

pub struct TcpOrUnixConnection;

impl TcpOrUnixConnection {
    pub const READ_BUFFER_CAPACITY: usize = 64 * 1024;
}

impl ConnectionKind for TcpOrUnixConnection {
    type Sink = IoVecThrottledSink<Writer>;
    type Stream = ReaderStream<Reader>;

    const DISPLAY_NAME: &'static str = "TCP/UNIX_STREAM";

    async fn connect(
        addr: &SocketAddress,
        target_pid: Option<u64>,
    ) -> io::Result<NewConnection<Self>> {
        let (reader, writer) = match addr {
            SocketAddress::Ip(addr) => {
                let stream = TcpStream::connect(addr).await?;
                // Writes on this socket are chunks relayed from the local application,
                // which already went through its own socket. If possible, set TCP_NODELAY.
                if let Err(error) = stream.set_nodelay(true) {
                    tracing::warn!(
                        %error,
                        peer_addr = %addr,
                        "Failed to set TCP_NODELAY on an outgoing TCP connection socket"
                    );
                }
                let (read, write) = stream.into_split();
                (Reader::Tcp(read), Writer::Tcp(write))
            }

            SocketAddress::Unix(UnixAddr::Pathname(path)) => {
                // In order to connect to a unix socket on the target pod, instead of connecting to
                // /the/target/path we connect to /proc/<PID>/root/the/target/path.
                let path = if let Some(pid) = target_pid {
                    InTargetPathResolver::new(pid).resolve(path)?
                } else {
                    path.clone()
                };
                let stream = UnixStream::connect(path).await?;
                let (read, write) = stream.into_split();
                (Reader::Unix(read), Writer::Unix(write))
            }

            SocketAddress::Unix(UnixAddr::Abstract(name)) => {
                // Abstract names are "paths" that start with a NUL byte.
                let mut name = name.clone();
                name.insert(0, 0);
                let path = OsStr::from_bytes(&name);
                let stream = UnixStream::connect(path).await?;
                let (read, write) = stream.into_split();
                (Reader::Unix(read), Writer::Unix(write))
            }

            SocketAddress::Unix(UnixAddr::Unnamed) => {
                return Err(io::Error::other("unexpected unnamed UNIX address"));
            }
        };

        fn convert_unix_addr(addr: unix::SocketAddr) -> UnixAddr {
            if let Some(path) = addr.as_pathname() {
                UnixAddr::Pathname(path.to_path_buf())
            } else if let Some(name) = addr.as_abstract_name() {
                UnixAddr::Abstract(name.to_vec())
            } else {
                UnixAddr::Unnamed
            }
        }

        let local_addr = match &reader {
            Reader::Tcp(stream) => SocketAddress::Ip(stream.local_addr()?),
            Reader::Unix(stream) => SocketAddress::Unix(convert_unix_addr(stream.local_addr()?)),
        };
        let peer_addr = match &reader {
            Reader::Tcp(stream) => SocketAddress::Ip(stream.peer_addr()?),
            Reader::Unix(stream) => SocketAddress::Unix(convert_unix_addr(stream.peer_addr()?)),
        };

        Ok(NewConnection {
            sink: IoVecThrottledSink::new(writer),
            stream: ReaderStream::with_capacity(reader, Self::READ_BUFFER_CAPACITY),
            local_addr,
            peer_addr,
        })
    }

    fn conn_counter() -> &'static AtomicUsize {
        &metrics::TCP_OUTGOING_CONNECTION
    }
}

impl OutgoingKind for TcpOrUnixConnection {
    type InMessage = LayerTcpOutgoing;

    fn transform_in(message: Self::InMessage) -> GenericInMessage {
        match message {
            LayerTcpOutgoing::Connect(layer_connect) => {
                GenericInMessage::ConnectLegacy(layer_connect.remote_address)
            }
            LayerTcpOutgoing::Write(layer_write) => {
                GenericInMessage::Write(layer_write.connection_id, layer_write.bytes.0)
            }
            LayerTcpOutgoing::Close(layer_close) => {
                GenericInMessage::Close(layer_close.connection_id)
            }
            LayerTcpOutgoing::ConnectV2(layer_connect_v2) => {
                GenericInMessage::Connect(layer_connect_v2.uid, layer_connect_v2.remote_address)
            }
        }
    }

    fn transform_out(message: GenericOutMessage) -> DaemonMessage {
        match message {
            GenericOutMessage::ConnectOk {
                uid: None,
                id,
                local_addr,
                peer_addr,
            } => DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Connect(Ok(DaemonConnect {
                connection_id: id,
                remote_address: peer_addr,
                local_address: local_addr,
            }))),
            GenericOutMessage::ConnectOk {
                uid: Some(uid),
                id,
                local_addr,
                peer_addr,
            } => DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::ConnectV2(DaemonConnectV2 {
                uid,
                connect: Ok(DaemonConnect {
                    connection_id: id,
                    local_address: local_addr,
                    remote_address: peer_addr,
                }),
            })),
            GenericOutMessage::ConnectErr { uid: None, error } => {
                DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Connect(Err(error.into())))
            }
            GenericOutMessage::ConnectErr {
                uid: Some(uid),
                error,
            } => DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::ConnectV2(DaemonConnectV2 {
                uid,
                connect: Err(error.into()),
            })),
            GenericOutMessage::Read(id, bytes) => {
                DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Read(Ok(DaemonRead {
                    connection_id: id,
                    bytes: bytes.into(),
                })))
            }
            GenericOutMessage::Close(id) => {
                DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Close(id))
            }
        }
    }
}

pub enum Writer {
    Tcp(tcp::OwnedWriteHalf),
    Unix(unix::OwnedWriteHalf),
}

impl AsyncWrite for Writer {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Self::Tcp(stream) => Pin::new(stream).poll_write(cx, buf),
            Self::Unix(stream) => Pin::new(stream).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Tcp(stream) => Pin::new(stream).poll_flush(cx),
            Self::Unix(stream) => Pin::new(stream).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Tcp(stream) => Pin::new(stream).poll_shutdown(cx),
            Self::Unix(stream) => Pin::new(stream).poll_shutdown(cx),
        }
    }

    fn is_write_vectored(&self) -> bool {
        match self {
            Self::Tcp(stream) => stream.is_write_vectored(),
            Self::Unix(stream) => stream.is_write_vectored(),
        }
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Self::Tcp(stream) => Pin::new(stream).poll_write_vectored(cx, bufs),
            Self::Unix(stream) => Pin::new(stream).poll_write_vectored(cx, bufs),
        }
    }
}

pub enum Reader {
    Tcp(tcp::OwnedReadHalf),
    Unix(unix::OwnedReadHalf),
}

impl AsyncRead for Reader {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Tcp(stream) => Pin::new(stream).poll_read(cx, buf),
            Self::Unix(stream) => Pin::new(stream).poll_read(cx, buf),
        }
    }
}
