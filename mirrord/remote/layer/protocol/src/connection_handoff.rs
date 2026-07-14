use std::{
    collections::HashSet,
    io::{Cursor, Error, ErrorKind, Read, Write},
    mem::ManuallyDrop,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream as StdTcpStream},
    os::unix::{
        io::{AsRawFd, FromRawFd, RawFd},
        net::UnixStream,
    },
    path::PathBuf,
    sync::{Arc, Mutex},
};

use tokio::net::{
    unix::SocketAddr as UnixSocketAddr, TcpListener, TcpStream as TokioTcpStream, UnixListener,
    UnixStream as TokioUnixStream,
};

use crate::{
    error::Result, ConnectionHandoffRequest, ConnectionHandoffResponse, ConnectionHandoffVerdict,
};

/// Environment variable used by the layer and sidecar to exchange accepted sockets over the
/// connection handoff socket.
pub const CONNECTION_HANDOFF_SOCKET_ENV: &str = "MIRRORD_REMOTE_HANDOFF_SOCKET";

/// Default location of the connection handoff socket. see [`CONNECTION_HANDOFF_SOCKET_ENV`]
const DEFAULT_CONNECTION_HANDOFF_SOCKET: &str = "/tmp/mirrord-remote-handoff.sock";

/// Shared read-only view of the destination ports currently subscribed for remote-layer traffic.
#[derive(Clone, Debug)]
pub struct RemoteLayerSubscriptionsView(Arc<Mutex<HashSet<u16>>>);

impl RemoteLayerSubscriptionsView {
    pub fn new(subscriptions: Arc<Mutex<HashSet<u16>>>) -> Self {
        Self(subscriptions)
    }

    pub fn contains(&self, port: u16) -> bool {
        self.0
            .lock()
            .expect("remote-layer subscription lock failed")
            .contains(&port)
    }
}

/// Owns the Unix listener used for connection handoff traffic.
pub struct ConnectionHandoffServer {
    listener: UnixListener,
    socket_path: PathBuf,
}

pub struct ConnectionHandoff {
    pub original_stream: TokioTcpStream,
    pub info: ConnectionHandoffRequest,
    pub passthrough_stream: TokioTcpStream,
}

impl ConnectionHandoffServer {
    pub fn bind() -> Result<Self> {
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

    pub async fn accept(&self) -> Result<(TokioUnixStream, UnixSocketAddr)> {
        Ok(self.listener.accept().await?)
    }
}

pub async fn handle_connection_handoff_connection(
    stream: TokioUnixStream,
    subscriptions: RemoteLayerSubscriptionsView,
) -> Result<Option<ConnectionHandoff>> {
    let unix_stream = stream.into_std()?;
    let ReceivedConnectionHandoff {
        request,
        accepted_fd,
    } = receive_connection_handoff_with_fd(&unix_stream)?;

    // Take ownership of the transferred raw fd so this function controls when it is closed
    // and we do not leak the accepted socket.
    let std_stream = unsafe { StdTcpStream::from_raw_fd(accepted_fd) };

    let local_address = socket_local_addr(accepted_fd)?;
    if local_address != request.local_address {
        tracing::trace!(
            accept_id = request.accept_id,
            expected_local_address = %request.local_address,
            observed_local_address = %local_address,
            "connection handoff local address differs from transferred metadata"
        );
    }

    // If the agent is not listening on this port, tell the layer to keep the socket
    // locally instead of creating a placeholder listener for it.
    if !subscriptions.contains(request.listener_address.port()) {
        send_connection_handoff_response(
            &unix_stream,
            &request,
            ConnectionHandoffVerdict::Rejected,
            local_address,
        )?;

        return Ok(None);
    }

    std_stream.set_nonblocking(true)?;
    let original_stream = TokioTcpStream::from_std(std_stream)?;

    let passthrough_stream = {
        let listener = create_placeholder_listener(request.listener_address).await?;
        let placeholder_address = listener.local_addr()?;
        send_connection_handoff_response(
            &unix_stream,
            &request,
            ConnectionHandoffVerdict::Accepted {
                placeholder_address,
            },
            local_address,
        )?;

        let (passthrough_stream, _) = listener.accept().await?;
        passthrough_stream
    };

    Ok(Some(ConnectionHandoff {
        original_stream,
        info: request,
        passthrough_stream,
    }))
}

impl Drop for ConnectionHandoffServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

struct ReceivedConnectionHandoff {
    request: ConnectionHandoffRequest,
    accepted_fd: RawFd,
}

fn receive_connection_handoff_with_fd(stream: &UnixStream) -> Result<ReceivedConnectionHandoff> {
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
    request: &ConnectionHandoffRequest,
    verdict: ConnectionHandoffVerdict,
    local_address: SocketAddr,
) -> Result<()> {
    let cursor = Cursor::new(Vec::new());
    let mut encoder = mirrord_intproxy_protocol::codec::SyncEncoder::new(cursor);
    encoder.send(&ConnectionHandoffResponse {
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

fn socket_local_addr(fd: RawFd) -> Result<SocketAddr> {
    let stream = ManuallyDrop::new(unsafe { StdTcpStream::from_raw_fd(fd) });
    Ok(stream.local_addr()?)
}

async fn create_placeholder_listener(address: SocketAddr) -> Result<TcpListener> {
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

pub fn prepare_handoff_socket() -> Result<PathBuf> {
    let path = std::env::var(CONNECTION_HANDOFF_SOCKET_ENV).unwrap_or_else(|_| {
        let default = DEFAULT_CONNECTION_HANDOFF_SOCKET.to_owned();
        // set default value into the env var for later spawn to inherit it
        unsafe { std::env::set_var(CONNECTION_HANDOFF_SOCKET_ENV, &default) };
        default
    });
    let path = PathBuf::from(&path);
    // if a stale version exists, remove it for the UnixListener::bind to create it
    match path.try_exists() {
        Ok(true) => {
            tracing::debug!(?path, "removing stale connection handoff socket");
            let _ = std::fs::remove_file(&path).inspect_err(
                |err| tracing::error!(%err, ?path, "failed to remove stale connection handoff socket"),
            );
        }
        Ok(false) => {
            tracing::trace!("verified connection handoff socket doesnt exist successfully");
        }
        Err(err) => tracing::warn!(%err, "error ensuring connection handoff socket doesnt exist"),
    };
    Ok(path)
}
