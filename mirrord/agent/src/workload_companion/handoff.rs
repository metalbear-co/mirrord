use std::{
    io::{Error, ErrorKind, IoSliceMut},
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream as StdTcpStream},
    os::unix::io::OwnedFd,
    path::PathBuf,
    time::Duration,
};

use mirrord_remote_layer_protocol::{
    CONNECTION_HANDOFF_SOCKET_ENV, ConnectionHandoffRequest, ConnectionHandoffResponse,
    ConnectionHandoffVerdict,
};
use tokio::{
    net::{TcpListener, TcpStream as TokioTcpStream},
    task::JoinSet,
    time::timeout,
};
use tokio_seqpacket::{UnixSeqpacket, UnixSeqpacketListener, ancillary::OwnedAncillaryMessage};
use tokio_util::sync::CancellationToken;

use super::{error::Result, incoming::IncomingConnectionSender};
use crate::incoming::Redirected;

// The peer may disappear at any stage, including after receiving the placeholder address.
const NEGOTIATION_TIMEOUT: Duration = Duration::from_secs(30);

/// Requests contain only an id and three socket addresses; allow ample encoding headroom while
/// still bounding the allocation for an untrusted peer.
const MAX_REQUEST_SIZE: usize = 8192;

/// Owns the Unix seqpacket listener used for connection handoff traffic.
///
/// Seqpacket sockets preserve message boundaries and carry the accepted TCP socket's fd alongside
/// its request in a single atomic send/receive, so negotiation needs neither a framing protocol
/// nor raw, manually-retried `recvmsg` calls to receive it.
pub(super) struct ConnectionHandoffServer {
    listener: UnixSeqpacketListener,
    socket_path: PathBuf,
    sender: IncomingConnectionSender,
}

impl ConnectionHandoffServer {
    pub(super) fn bind(sender: IncomingConnectionSender) -> Result<Self> {
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
        let listener = UnixSeqpacketListener::bind(&socket_path)?;

        Ok(Self {
            listener,
            socket_path: socket_path.into(),
            sender,
        })
    }

    pub(super) async fn run(mut self, cancellation_token: CancellationToken) -> Result<()> {
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
                    let stream = accepted?;
                    tracing::trace!("accepted connection handoff connection");
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

    fn spawn_connection(&self, connections: &mut JoinSet<()>, stream: UnixSeqpacket) {
        let sender = self.sender.clone();

        connections.spawn(async move {
            if let Err(error) = Self::serve_connection(stream, sender).await {
                tracing::warn!(%error, "connection handoff failed");
            }
        });
    }

    async fn serve_connection(
        stream: UnixSeqpacket,
        sender: IncomingConnectionSender,
    ) -> Result<()> {
        let accepted = Handoff { stream }.negotiate().await?;

        accepted.original_stream.set_nonblocking(true)?;
        let original_stream = TokioTcpStream::from_std(accepted.original_stream)?;
        let destination = original_stream
            .local_addr()
            .unwrap_or(accepted.request.local_address);
        let connection = Redirected::new(
            original_stream,
            accepted.request.peer_address,
            destination,
            Some(accepted.passthrough_stream),
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

/// Owns negotiation sockets so cancellation closes them without detached work.
struct Handoff {
    stream: UnixSeqpacket,
}

/// Successful handoff result ready for delivery to the incoming pipeline.
struct AcceptedConnection {
    original_stream: StdTcpStream,
    request: ConnectionHandoffRequest,
    passthrough_stream: TokioTcpStream,
}

impl Handoff {
    async fn negotiate(self) -> Result<AcceptedConnection> {
        timeout(NEGOTIATION_TIMEOUT, self.negotiate_inner())
            .await
            .map_err(|_| {
                Error::new(
                    ErrorKind::TimedOut,
                    "connection handoff negotiation timed out",
                )
            })?
    }

    async fn negotiate_inner(&self) -> Result<AcceptedConnection> {
        let (request, accepted_fd) = self.receive_request().await?;
        let original_stream: StdTcpStream = accepted_fd.into();
        let local_address = original_stream.local_addr()?;
        Self::log_local_address_mismatch(&request, local_address);

        let listener = Self::create_placeholder_listener(request.listener_address).await?;
        let placeholder_address = listener.local_addr()?;
        self.send_response(
            &request,
            ConnectionHandoffVerdict::Accepted {
                placeholder_address,
            },
            local_address,
        )
        .await?;
        let (passthrough_stream, _) = listener.accept().await?;

        Ok(AcceptedConnection {
            original_stream,
            request,
            passthrough_stream,
        })
    }

    /// A seqpacket request always arrives as one complete, self-contained message together with
    /// its ancillary fd, so unlike a stream socket this needs no framing and no possibility of a
    /// request spanning multiple reads.
    async fn receive_request(&self) -> Result<(ConnectionHandoffRequest, OwnedFd)> {
        let mut buffer = vec![0u8; MAX_REQUEST_SIZE];
        let mut iov = [IoSliceMut::new(&mut buffer)];
        let mut ancillary_buffer = [0u8; 128];
        let (bytes, ancillary) = self
            .stream
            .recv_vectored_with_ancillary(&mut iov, &mut ancillary_buffer)
            .await?;

        if ancillary.is_truncated() {
            return Err(
                Error::new(ErrorKind::InvalidData, "connection handoff fd truncated").into(),
            );
        }

        let accepted_fd = ancillary
            .into_messages()
            .find_map(|message| match message {
                OwnedAncillaryMessage::FileDescriptors(mut fds) => fds.next(),
                _ => None,
            })
            .ok_or_else(|| Error::new(ErrorKind::InvalidData, "missing accepted socket fd"))?;

        let received = buffer.get(..bytes).ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidData,
                "received more bytes than fit in the connection handoff buffer",
            )
        })?;
        let request = decode_handoff_message(received)?;

        Ok((request, accepted_fd))
    }

    async fn send_response(
        &self,
        request: &ConnectionHandoffRequest,
        verdict: ConnectionHandoffVerdict,
        local_address: SocketAddr,
    ) -> Result<()> {
        let frame = encode_handoff_message(&ConnectionHandoffResponse {
            accept_id: request.accept_id,
            verdict,
            listener_address: request.listener_address,
            local_address,
            peer_address: request.peer_address,
        })?;

        let sent = self.stream.send(&frame).await?;
        if sent < frame.len() {
            return Err(Error::new(
                ErrorKind::WriteZero,
                "failed to send the whole connection handoff response",
            )
            .into());
        }

        Ok(())
    }

    async fn create_placeholder_listener(address: SocketAddr) -> Result<TcpListener> {
        let localhost = if address.is_ipv4() {
            Ipv4Addr::LOCALHOST.into()
        } else {
            Ipv6Addr::LOCALHOST.into()
        };

        Ok(TcpListener::bind(SocketAddr::new(localhost, 0)).await?)
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

/// Encodes a handoff request or response into a single self-contained buffer.
///
/// Handoff messages travel as whole `SOCK_SEQPACKET` datagrams, so unlike
/// `mirrord_intproxy_protocol`'s stream codec, no length prefix is needed to find the message
/// boundary; the socket already provides one.
fn encode_handoff_message<T: bincode::Encode>(value: &T) -> Result<Vec<u8>> {
    bincode::encode_to_vec(value, bincode::config::standard())
        .map_err(|error| Error::new(ErrorKind::InvalidData, error).into())
}

/// Decodes a handoff request or response from a single, complete datagram.
fn decode_handoff_message<T: bincode::Decode<()>>(bytes: &[u8]) -> Result<T> {
    let (value, consumed) = bincode::decode_from_slice(bytes, bincode::config::standard())
        .map_err(|error| Error::new(ErrorKind::InvalidData, error))?;
    if consumed != bytes.len() {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "connection handoff message has leftover bytes",
        )
        .into());
    }

    Ok(value)
}

#[cfg(test)]
mod tests {
    use std::{io::IoSlice, os::unix::io::AsFd};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_seqpacket::ancillary::AncillaryMessageWriter;

    use super::*;

    const TEST_TIMEOUT: Duration = Duration::from_secs(2);

    async fn fixture() -> (
        Handoff,
        UnixSeqpacket,
        TokioTcpStream,
        TokioTcpStream,
        Vec<u8>,
    ) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let peer = TokioTcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (original, _) = listener.accept().await.unwrap();
        let request = ConnectionHandoffRequest {
            accept_id: 42,
            listener_address: listener.local_addr().unwrap(),
            local_address: original.local_addr().unwrap(),
            peer_address: original.peer_addr().unwrap(),
        };
        let frame = encode_handoff_message(&request).unwrap();
        let (stream, client) = UnixSeqpacket::pair().unwrap();
        (Handoff { stream }, client, original, peer, frame)
    }

    /// A seqpacket send delivers the whole request and its ancillary fd atomically, so unlike a
    /// stream socket there's no partial-write case to fall back on.
    async fn send_fd(client: &UnixSeqpacket, original: &TokioTcpStream, bytes: &[u8]) {
        let mut ancillary_buffer = [0u8; 128];
        let mut ancillary = AncillaryMessageWriter::new(&mut ancillary_buffer);
        ancillary.add_fds(&[original.as_fd()]).unwrap();
        let sent = client
            .send_vectored_with_ancillary(&[IoSlice::new(bytes)], &mut ancillary)
            .await
            .unwrap();
        assert_eq!(sent, bytes.len());
    }

    async fn response(client: &UnixSeqpacket) -> ConnectionHandoffResponse {
        let mut buffer = [0u8; MAX_REQUEST_SIZE];
        let bytes = timeout(TEST_TIMEOUT, client.recv(&mut buffer))
            .await
            .unwrap()
            .unwrap();
        decode_handoff_message(buffer.get(..bytes).unwrap()).unwrap()
    }

    async fn assert_closed(stream: &mut (impl tokio::io::AsyncRead + Unpin)) {
        assert_eq!(
            timeout(TEST_TIMEOUT, stream.read(&mut [0]))
                .await
                .unwrap()
                .unwrap(),
            0
        );
    }

    async fn assert_seqpacket_closed(stream: &UnixSeqpacket) {
        assert_eq!(
            timeout(TEST_TIMEOUT, stream.recv(&mut [0]))
                .await
                .unwrap()
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn cancellation_closes_idle_unix_socket() {
        let (handoff, client, _, _, _) = fixture().await;
        let mut tasks = JoinSet::new();
        tasks.spawn(handoff.negotiate());
        tokio::task::yield_now().await;
        timeout(TEST_TIMEOUT, tasks.shutdown()).await.unwrap();
        assert_seqpacket_closed(&client).await;
    }

    #[tokio::test]
    async fn cancellation_closes_placeholder_listener_and_transferred_socket() {
        let (handoff, client, original, mut peer, frame) = fixture().await;
        send_fd(&client, &original, &frame).await;
        drop(original);
        let mut tasks = JoinSet::new();
        tasks.spawn(handoff.negotiate());
        let ConnectionHandoffVerdict::Accepted {
            placeholder_address,
        } = response(&client).await.verdict
        else {
            panic!("handoff rejected");
        };
        timeout(TEST_TIMEOUT, tasks.shutdown()).await.unwrap();
        assert_seqpacket_closed(&client).await;
        assert_closed(&mut peer).await;
        assert!(
            timeout(TEST_TIMEOUT, TokioTcpStream::connect(placeholder_address))
                .await
                .unwrap()
                .is_err()
        );
    }

    #[tokio::test]
    async fn handoff_completes() {
        let (handoff, client, original, mut peer, frame) = fixture().await;
        send_fd(&client, &original, &frame).await;
        drop(original);
        let task = tokio::spawn(handoff.negotiate());
        let response = response(&client).await;
        assert_eq!(response.accept_id, 42);
        let ConnectionHandoffVerdict::Accepted {
            placeholder_address,
        } = response.verdict
        else {
            panic!("handoff rejected");
        };
        let mut placeholder = TokioTcpStream::connect(placeholder_address).await.unwrap();
        let accepted = timeout(TEST_TIMEOUT, task).await.unwrap().unwrap().unwrap();
        accepted.original_stream.set_nonblocking(true).unwrap();
        let mut original = TokioTcpStream::from_std(accepted.original_stream).unwrap();
        let mut passthrough = accepted.passthrough_stream;
        peer.write_all(b"a").await.unwrap();
        let mut byte = [0];
        timeout(TEST_TIMEOUT, original.read_exact(&mut byte))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&byte, b"a");
        passthrough.write_all(b"b").await.unwrap();
        timeout(TEST_TIMEOUT, placeholder.read_exact(&mut byte))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&byte, b"b");
    }

    #[tokio::test]
    async fn handoff_is_accepted() {
        let (handoff, client, original, _, frame) = fixture().await;
        send_fd(&client, &original, &frame).await;
        drop(original);
        let task = tokio::spawn(handoff.negotiate());
        let ConnectionHandoffVerdict::Accepted {
            placeholder_address,
        } = response(&client).await.verdict
        else {
            panic!("handoff rejected");
        };
        let _placeholder = TokioTcpStream::connect(placeholder_address).await.unwrap();
        timeout(TEST_TIMEOUT, task).await.unwrap().unwrap().unwrap();
    }

    #[tokio::test]
    async fn stalled_placeholder_negotiation_times_out() {
        let (handoff, client, original, mut peer, frame) = fixture().await;
        send_fd(&client, &original, &frame).await;
        drop(original);
        let task = tokio::spawn(handoff.negotiate());
        assert!(matches!(
            response(&client).await.verdict,
            ConnectionHandoffVerdict::Accepted { .. }
        ));
        let result = timeout(NEGOTIATION_TIMEOUT + TEST_TIMEOUT, task)
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(result, Err(super::super::error::RemoteIncomingHandoffError::Io(error)) if error.kind() == ErrorKind::TimedOut)
        );
        assert_seqpacket_closed(&client).await;
        assert_closed(&mut peer).await;
    }
}
