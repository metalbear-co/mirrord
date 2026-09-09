use std::{
    io::{self, Cursor, Error, ErrorKind, Read, Write},
    net::{
        Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener as StdTcpListener, TcpStream as StdTcpStream,
    },
    os::unix::{
        io::{AsRawFd, FromRawFd, OwnedFd, RawFd},
        net::UnixStream,
    },
    path::PathBuf,
};

use mirrord_intproxy_protocol::codec::{SyncDecoder, SyncEncoder};
use mirrord_remote_layer_protocol::{
    CONNECTION_HANDOFF_SOCKET_ENV, ConnectionHandoffRequest, ConnectionHandoffResponse,
    ConnectionHandoffVerdict, error::Result,
};
use nix::{
    errno::Errno,
    sys::socket::{ControlMessageOwned, MsgFlags, recvmsg},
};
use tokio::{
    net::{TcpStream as TokioTcpStream, UnixListener, UnixStream as TokioUnixStream},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

use super::incoming::{IncomingConnectionSender, SubscribedPorts};
use crate::incoming::Redirected;

/// Owns the Unix listener used for connection handoff traffic.
pub(super) struct ConnectionHandoffServer {
    listener: UnixListener,
    socket_path: PathBuf,
    sender: IncomingConnectionSender,
    subscriptions: SubscribedPorts,
}

impl ConnectionHandoffServer {
    pub(super) fn bind(
        sender: IncomingConnectionSender,
        subscriptions: SubscribedPorts,
    ) -> Result<Self> {
        let socket_path = std::env::var(CONNECTION_HANDOFF_SOCKET_ENV).map_err(|error| {
            Error::new(
                ErrorKind::NotFound,
                format!(
                    "missing {} environment variable: {error}",
                    CONNECTION_HANDOFF_SOCKET_ENV
                ),
            )
        })?;

        // Bootstrap allocates a unique run directory before starting the agent.
        let listener = UnixListener::bind(&socket_path)?;

        Ok(Self {
            listener,
            socket_path: socket_path.into(),
            sender,
            subscriptions,
        })
    }

    pub(super) async fn run(self, cancellation_token: CancellationToken) -> Result<()> {
        let mut connections = JoinSet::new();

        loop {
            tokio::select! {
                // Stop accepting handoffs and terminate all in-flight connection tasks when the
                // workload companion shuts down.
                _ = cancellation_token.cancelled() => {
                    connections.shutdown().await;
                    return Ok(());
                }
                // Handle handoffs concurrently because each accepted connection can block while
                // waiting for the remote layer to connect its placeholder socket.
                accepted = self.listener.accept() => {
                    let (stream, peer) = accepted?;
                    tracing::trace!(peer = ?peer, "accepted connection handoff connection");
                    self.spawn_connection(&mut connections, stream);
                }
                // Reap completed connection tasks so panics and cancellations remain visible
                // without allowing the join set to grow for the server's lifetime.
                joined = connections.join_next(), if !connections.is_empty() => {
                    if let Some(Err(error)) = joined {
                        tracing::error!(%error, "connection handoff task failed to join");
                    }
                }
            }
        }
    }

    fn spawn_connection(&self, connections: &mut JoinSet<()>, stream: TokioUnixStream) {
        let sender = self.sender.clone();
        let subscriptions = self.subscriptions.clone();

        connections.spawn(async move {
            if let Err(error) = Self::serve_connection(stream, sender, subscriptions).await {
                tracing::error!(%error, "connection handoff failed");
            }
        });
    }

    async fn serve_connection(
        stream: TokioUnixStream,
        sender: IncomingConnectionSender,
        subscriptions: SubscribedPorts,
    ) -> Result<()> {
        let stream = stream.into_std()?;
        stream.set_nonblocking(false)?;

        let accepted = tokio::task::spawn_blocking(move || {
            BlockingHandoff {
                stream,
                subscriptions,
            }
            .negotiate()
        })
        .await
        .map_err(|error| Error::other(format!("connection handoff task failed: {error}")))??;

        let Some(accepted) = accepted else {
            tracing::trace!("declined remote accept handoff");
            return Ok(());
        };

        accepted.original_stream.set_nonblocking(true)?;
        accepted.passthrough_stream.set_nonblocking(true)?;

        let original_stream = TokioTcpStream::from_std(accepted.original_stream)?;
        let passthrough_stream = TokioTcpStream::from_std(accepted.passthrough_stream)?;
        let destination = original_stream
            .local_addr()
            .unwrap_or(accepted.request.local_address);
        let connection = Redirected::new(
            original_stream,
            accepted.request.peer_address,
            destination,
            Some(passthrough_stream),
        );

        sender.send(connection).await.map_err(|_| {
            Error::new(
                ErrorKind::BrokenPipe,
                "remote ingress channel closed while sending accepted handoff",
            )
            .into()
        })
    }
}

impl Drop for ConnectionHandoffServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

/// Performs one complete handoff negotiation using blocking Unix and TCP sockets.
struct BlockingHandoff {
    stream: UnixStream,
    subscriptions: SubscribedPorts,
}

/// Successful blocking handoff result returned to the async server for delivery.
struct AcceptedConnection {
    original_stream: StdTcpStream,
    request: ConnectionHandoffRequest,
    passthrough_stream: StdTcpStream,
}

impl BlockingHandoff {
    fn negotiate(self) -> Result<Option<AcceptedConnection>> {
        let (request, accepted_fd) = self.receive_request()?;
        let original_stream: StdTcpStream = accepted_fd.into();
        let local_address = original_stream.local_addr()?;
        Self::log_local_address_mismatch(&request, local_address);

        if !self
            .subscriptions
            .contains(request.listener_address.port())?
        {
            self.send_response(&request, ConnectionHandoffVerdict::Rejected, local_address)?;
            return Ok(None);
        }

        let listener = Self::create_placeholder_listener(request.listener_address)?;
        let placeholder_address = listener.local_addr()?;
        self.send_response(
            &request,
            ConnectionHandoffVerdict::Accepted {
                placeholder_address,
            },
            local_address,
        )?;
        let (passthrough_stream, _) = listener.accept()?;

        Ok(Some(AcceptedConnection {
            original_stream,
            request,
            passthrough_stream,
        }))
    }

    fn receive_request(&self) -> Result<(ConnectionHandoffRequest, OwnedFd)> {
        let mut buffer = vec![0u8; 8192];
        let (accepted_fd, bytes) = loop {
            let mut cmsgspace = nix::cmsg_space!([RawFd; 1]);
            let mut iov = [std::io::IoSliceMut::new(&mut buffer)];

            match recvmsg::<()>(
                self.stream.as_raw_fd(),
                &mut iov,
                Some(&mut cmsgspace),
                MsgFlags::empty(),
            ) {
                Ok(message) => {
                    let accepted_fd = message
                        .cmsgs()
                        .map_err(|error| io::Error::from_raw_os_error(error as i32))?
                        .find_map(|control_message| match control_message {
                            ControlMessageOwned::ScmRights(fds) => fds
                                .into_iter()
                                .next()
                                .map(|fd| unsafe { OwnedFd::from_raw_fd(fd) }),
                            _ => None,
                        })
                        .ok_or_else(|| {
                            Error::new(ErrorKind::InvalidData, "missing accepted socket fd")
                        })?;

                    break (accepted_fd, message.bytes);
                }
                Err(Errno::EINTR) => continue,
                Err(error) => return Err(io::Error::from_raw_os_error(error as i32).into()),
            }
        };

        let buffer = buffer.get(..bytes).ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidData,
                "received more bytes than fit in the connection handoff buffer",
            )
        })?;
        let mut decoder = SyncDecoder::new(PrefixedReader::new(buffer, self.stream.try_clone()?));
        let request = decoder.receive()?.ok_or_else(|| {
            Error::new(
                ErrorKind::UnexpectedEof,
                "missing connection handoff request",
            )
        })?;

        Ok((request, accepted_fd))
    }

    fn send_response(
        &self,
        request: &ConnectionHandoffRequest,
        verdict: ConnectionHandoffVerdict,
        local_address: SocketAddr,
    ) -> Result<()> {
        let cursor = Cursor::new(Vec::new());
        let mut encoder = SyncEncoder::new(cursor);
        encoder.send(&ConnectionHandoffResponse {
            accept_id: request.accept_id,
            verdict,
            listener_address: request.listener_address,
            local_address,
            peer_address: request.peer_address,
        })?;

        let frame = encoder.into_inner().into_inner();
        let mut writer = self.stream.try_clone()?;
        writer.write_all(&frame)?;
        writer.flush()?;
        Ok(())
    }

    fn create_placeholder_listener(address: SocketAddr) -> Result<StdTcpListener> {
        let localhost = if address.is_ipv4() {
            Ipv4Addr::LOCALHOST.into()
        } else {
            Ipv6Addr::LOCALHOST.into()
        };

        Ok(StdTcpListener::bind(SocketAddr::new(localhost, 0))?)
    }

    fn log_local_address_mismatch(request: &ConnectionHandoffRequest, observed: SocketAddr) {
        if observed != request.local_address {
            tracing::warn!(
                accept_id = request.accept_id,
                expected_local_address = %request.local_address,
                observed_local_address = %observed,
                "connection handoff local address differs from transferred metadata"
            );
        }
    }
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
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.prefix.read(buf)?;
        if read > 0 {
            return Ok(read);
        }

        self.reader.read(buf)
    }
}
