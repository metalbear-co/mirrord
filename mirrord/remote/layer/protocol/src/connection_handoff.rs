use std::{
    io::{Cursor, Error, ErrorKind, Read, Write},
    mem::ManuallyDrop,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream as StdTcpStream},
    os::unix::{
        io::{AsRawFd, FromRawFd, RawFd},
        net::UnixStream,
    },
    path::PathBuf,
};

use tokio::net::{
    TcpListener, TcpStream as TokioTcpStream, UnixListener, UnixStream as TokioUnixStream,
    unix::SocketAddr as UnixSocketAddr,
};

use crate::{
    AcceptHandoffRequest, AcceptHandoffResponse, RemoteAcceptVerdict,
    error::RemoteLayerProtocolError,
};

/// Environment variable used by the layer and sidecar to exchange accepted sockets over the
/// connection handoff socket.
pub const CONNECTION_HANDOFF_SOCKET_ENV: &str = "MIRRORD_REMOTE_HANDOFF_SOCKET";

/// Owns the Unix listener used for connection handoff traffic.
pub struct ConnectionHandoffServer {
    listener: UnixListener,
    socket_path: PathBuf,
}

pub struct ConnectionHandoff {
    pub original_stream: TokioTcpStream,
    pub info: AcceptHandoffRequest,
    pub passthrough_stream: TokioTcpStream,
}

impl ConnectionHandoffServer {
    pub fn bind() -> Result<Self, RemoteLayerProtocolError> {
        let socket_path = std::env::var(CONNECTION_HANDOFF_SOCKET_ENV).map_err(|error| {
            Error::new(
                ErrorKind::NotFound,
                format!(
                    "missing {} environment variable: {error}",
                    CONNECTION_HANDOFF_SOCKET_ENV
                ),
            )
        })?;

        let _ = std::fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path)?;

        Ok(Self {
            listener,
            socket_path: socket_path.into(),
        })
    }

    pub async fn accept(
        &self,
    ) -> Result<(TokioUnixStream, UnixSocketAddr), RemoteLayerProtocolError> {
        Ok(self.listener.accept().await?)
    }
}

pub async fn handle_connection_handoff_connection(
    stream: TokioUnixStream,
) -> Result<ConnectionHandoff, RemoteLayerProtocolError> {
    let unix_stream = stream.into_std()?;
    let ReceivedConnectionHandoff {
        request,
        accepted_fd,
    } = receive_connection_handoff_with_fd(&unix_stream)?;
    let local_address = socket_local_addr(accepted_fd)?;

    if local_address != request.local_address {
        tracing::trace!(
            accept_id = request.accept_id,
            expected_local_address = %request.local_address,
            observed_local_address = %local_address,
            "connection handoff local address differs from transferred metadata"
        );
    }

    let std_stream = unsafe { StdTcpStream::from_raw_fd(accepted_fd) };
    std_stream.set_nonblocking(true)?;
    let original_stream = TokioTcpStream::from_std(std_stream)?;

    let passthrough_stream = {
        let listener = create_placeholder_listener(request.listener_address).await?;
        let placeholder_address = listener.local_addr()?;
        send_connection_handoff_response(
            &unix_stream,
            &request,
            RemoteAcceptVerdict::Claim {
                placeholder_address,
            },
            local_address,
        )?;

        let (passthrough_stream, _) = listener.accept().await?;
        passthrough_stream
    };

    Ok(ConnectionHandoff {
        original_stream,
        info: request,
        passthrough_stream,
    })
}

impl Drop for ConnectionHandoffServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

struct ReceivedConnectionHandoff {
    request: AcceptHandoffRequest,
    accepted_fd: RawFd,
}

fn receive_connection_handoff_with_fd(
    stream: &UnixStream,
) -> Result<ReceivedConnectionHandoff, RemoteLayerProtocolError> {
    let mut buffer = vec![0u8; 8192];
    let (accepted_fd, bytes) = {
        let mut cmsgspace = nix::cmsg_space!([RawFd; 1]);
        let mut iov = [std::io::IoSliceMut::new(&mut buffer)];

        let message = nix::sys::socket::recvmsg::<()>(
            stream.as_raw_fd(),
            &mut iov,
            Some(&mut cmsgspace),
            nix::sys::socket::MsgFlags::empty(),
        )?;

        let accepted_fd = message
            .cmsgs()?
            .find_map(|control_message| match control_message {
                nix::sys::socket::ControlMessageOwned::ScmRights(fds) => fds.into_iter().next(),
                _ => None,
            })
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "missing accepted socket fd",
                )
            })?;

        (accepted_fd, message.bytes)
    };

    let mut decoder = mirrord_intproxy_protocol::codec::SyncDecoder::new(PrefixedReader::new(
        &buffer[..bytes],
        stream.try_clone()?,
    ));
    let request = decoder.receive()?.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "missing connection handoff request",
        )
    })?;

    Ok(ReceivedConnectionHandoff {
        request,
        accepted_fd,
    })
}

fn send_connection_handoff_response(
    stream: &UnixStream,
    request: &AcceptHandoffRequest,
    verdict: RemoteAcceptVerdict,
    local_address: SocketAddr,
) -> Result<(), RemoteLayerProtocolError> {
    let cursor = Cursor::new(Vec::new());
    let mut encoder = mirrord_intproxy_protocol::codec::SyncEncoder::new(cursor);
    encoder.send(&AcceptHandoffResponse {
        accept_id: request.accept_id,
        verdict,
        listener_address: request.listener_address,
        local_address,
        peer_address: request.peer_address,
    })?;

    let frame = encoder.into_inner().into_inner();
    let mut writer = stream.try_clone()?;
    writer.write_all(&frame)?;
    writer.flush()?;
    Ok(())
}

fn socket_local_addr(fd: RawFd) -> Result<SocketAddr, RemoteLayerProtocolError> {
    let stream = ManuallyDrop::new(unsafe { StdTcpStream::from_raw_fd(fd) });
    Ok(stream.local_addr()?)
}

async fn create_placeholder_listener(
    address: SocketAddr,
) -> Result<TcpListener, RemoteLayerProtocolError> {
    let localhost = if address.is_ipv4() {
        Ipv4Addr::LOCALHOST.into()
    } else {
        Ipv6Addr::LOCALHOST.into()
    };

    Ok(TcpListener::bind(SocketAddr::new(localhost, 0)).await?)
}

struct PrefixedReader<'a, R> {
    prefix: Cursor<&'a [u8]>,
    reader: R,
}

impl<'a, R> PrefixedReader<'a, R> {
    fn new(prefix: &'a [u8], reader: R) -> Self {
        Self {
            prefix: Cursor::new(prefix),
            reader,
        }
    }
}

impl<R: Read> Read for PrefixedReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let read = self.prefix.read(buf)?;
        if read > 0 {
            return Ok(read);
        }

        self.reader.read(buf)
    }
}
