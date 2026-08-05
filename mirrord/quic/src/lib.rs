//! QUIC transport for the connection between the mirrord Operator and mirrord-agent.
//!
//! # Why QUIC
//!
//! A single agent is shared by many sessions, and every session multiplexes all of its remote IO
//! (outgoing connections, stolen connections, file operations) onto one connection with the agent.
//! Over TCP, a large or slow message blocks every other message behind it, because the whole
//! multiplex shares one byte stream and one congestion window. QUIC gives each logical connection
//! its own stream, with independent ordering, flow control, and loss recovery.
//!
//! # Streams
//!
//! The first bidirectional stream, opened by the operator right after the handshake, is the
//! *control stream*. It carries the mirrord-protocol message exchange
//! ([`ClientMessage`](mirrord_protocol::ClientMessage) /
//! [`DaemonMessage`](mirrord_protocol::DaemonMessage)) framed exactly as it is over TCP, so both
//! ends can reuse their existing codecs.
//!
//! Every other stream is a *data stream*, carrying the raw bytes of one intercepted connection with
//! no framing and no per-chunk decode. Data streams are always opened by the **operator**, after
//! the control stream has told it that the connection exists. That ordering is what makes the
//! rendezvous trivial: by the time the agent accepts a data stream, the socket it belongs to is
//! already waiting, so neither end has to park a stream it cannot yet match. Until the stream
//! arrives the agent simply does not read from the socket, and the kernel's receive buffer plus
//! TCP backpressure hold whatever the peer sent in the meantime.
//!
//! A data stream also carries the connection's lifecycle, which keeps it ordered with the data
//! rather than racing it on the control stream:
//!
//! * Either side finishing its sending half means "no more data this way", the equivalent of a
//!   `shutdown(2)` on the intercepted socket.
//! * Resetting the stream means the connection is gone.
//!
//! # Trust model
//!
//! QUIC requires the side that dials to be the TLS client, which inverts the TLS roles used over
//! TCP: there, the agent accepts the TCP connection but acts as the TLS client, pinning the
//! operator certificate it was given in
//! [`OPERATOR_CERT`](mirrord_agent_env::envs::OPERATOR_CERT).
//!
//! The inversion is resolved with mutual TLS, which preserves the property the TCP setup relies
//! on - that only the operator can talk to the agent:
//!
//! * The agent presents an ephemeral self-signed certificate generated in-process on startup. Its
//!   private key never leaves the agent's pod, and nothing needs to provision it.
//! * The agent requires a client certificate and verifies it against the pinned operator
//!   certificate, so only the holder of the operator's private key can open a connection.
//! * The operator presents that certificate as its client certificate and accepts the agent's
//!   ephemeral certificate without verification. The agent is identified by the pod IP resolved
//!   from the Kubernetes API, which is how the TCP transport identifies it as well.

use std::{
    fmt, io,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use mirrord_tls_util::DangerousNoVerifierServer;
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use rustls::{
    RootCertStore,
    pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
    server::WebPkiClientVerifier,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, Join, ReadBuf};

mod error;

pub use error::{ControlStreamError, DataStreamError, QuicSetupError};

/// ALPN protocol negotiated on every operator to agent QUIC connection.
///
/// The suffix is the transport generation, not [`TRANSPORT_VERSION`]. It only changes if the
/// meaning of the streams themselves changes in a way that older peers cannot detect, which
/// [`ControlHeader`] version negotiation would be too late to catch.
pub const ALPN: &[u8] = b"mirrord-agent/1";

/// Highest version of the stream conventions this build understands.
///
/// Both ends exchange this in [`ControlHeader`] and continue with the lower of the two, so a newer
/// operator keeps working against an older agent and vice versa.
///
/// * `1` - control stream only. All remote IO is carried as mirrord-protocol messages.
/// * `2` - adds data streams for outgoing TCP connections.
pub const TRANSPORT_VERSION: u16 = 2;

/// First [`TRANSPORT_VERSION`] in which data streams exist.
pub const DATA_STREAM_VERSION: u16 = 2;

/// Identifies the control stream, so that a peer speaking something else on this port fails
/// immediately and legibly instead of as a protocol decode error further down.
const CONTROL_MAGIC: [u8; 6] = *b"mrdqic";

/// Subject alternate name of the agent's ephemeral certificate, and the server name the operator
/// passes to [`quinn::Endpoint::connect`].
///
/// The operator does not verify the agent's certificate, so this never has to match anything the
/// agent is reachable at. It is only ever seen in logs and in TLS-level errors.
pub const AGENT_SERVER_NAME: &str = "mirrord-agent";

/// How long a connection may be idle before either end tears it down.
///
/// The operator runs its own ping-pong over the control stream at a shorter interval, so reaching
/// this means the peer is gone rather than quiet.
const MAX_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// How often the operator sends QUIC-level keep-alives, well below [`MAX_IDLE_TIMEOUT`].
const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(10);

/// Upper bound on concurrently open streams in one direction.
///
/// Each remote connection handled for a session gets its own stream, so this bounds how many
/// connections a single session can have in flight.
const MAX_CONCURRENT_STREAMS: u32 = 1 << 16;

/// How much data one intercepted connection may have in flight before its sender is made to wait.
///
/// This is QUIC's per-stream flow control doing the job that the agent otherwise has to do by hand
/// with a semaphore around its reads: bound how much a connection can buffer without letting a busy
/// connection starve the others.
const STREAM_RECEIVE_WINDOW: u32 = 512 * 1024;

/// How much data all streams together may have in flight.
///
/// Bounds total memory per agent connection, so that many busy intercepted connections cannot add
/// up to an unbounded amount of buffered data.
const CONNECTION_RECEIVE_WINDOW: u32 = 16 * 1024 * 1024;

/// A bidirectional QUIC stream presented as a single byte stream.
///
/// Framed codecs need one type that is both [`AsyncRead`](tokio::io::AsyncRead) and
/// [`AsyncWrite`](tokio::io::AsyncWrite), while QUIC hands out the two halves separately.
pub type BiStream = Join<quinn::RecvStream, quinn::SendStream>;

/// Preamble exchanged on the control stream before any mirrord-protocol message.
///
/// The operator sends its header immediately after opening the stream, and the agent replies with
/// its own. Beyond negotiating [`TRANSPORT_VERSION`], the operator's header is what makes the
/// stream visible to the agent at all: QUIC only surfaces a newly opened stream to the peer once
/// something has been written to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlHeader {
    /// Highest version understood by the sender.
    pub version: u16,
}

impl ControlHeader {
    const LEN: usize = CONTROL_MAGIC.len() + size_of::<u16>();

    /// Header advertising this build's [`TRANSPORT_VERSION`].
    pub const CURRENT: Self = Self {
        version: TRANSPORT_VERSION,
    };

    fn encode(&self) -> [u8; Self::LEN] {
        let mut buffer = [0_u8; Self::LEN];
        buffer[..CONTROL_MAGIC.len()].copy_from_slice(&CONTROL_MAGIC);
        buffer[CONTROL_MAGIC.len()..].copy_from_slice(&self.version.to_be_bytes());
        buffer
    }

    fn decode(buffer: [u8; Self::LEN]) -> Result<Self, ControlStreamError> {
        let (magic, version) = buffer.split_at(CONTROL_MAGIC.len());
        if magic != CONTROL_MAGIC {
            return Err(ControlStreamError::BadMagic);
        }

        Ok(Self {
            version: u16::from_be_bytes(
                version
                    .try_into()
                    .expect("header remainder is exactly two bytes"),
            ),
        })
    }

    async fn write_to(&self, stream: &mut BiStream) -> Result<(), ControlStreamError> {
        stream.write_all(&self.encode()).await?;
        stream.flush().await?;
        Ok(())
    }

    async fn read_from(stream: &mut BiStream) -> Result<Self, ControlStreamError> {
        let mut buffer = [0_u8; Self::LEN];
        stream.read_exact(&mut buffer).await?;
        Self::decode(buffer)
    }
}

/// What kind of intercepted connection a data stream carries.
///
/// Sent on the wire as a single byte, so that a peer which learns about new kinds in a later
/// version can reject one it does not know instead of misreading the bytes that follow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DataStreamKind {
    /// A connection the intercepted process made to a remote address, which the agent established
    /// on its behalf.
    TcpOutgoing = 1,
}

impl DataStreamKind {
    fn from_byte(byte: u8) -> Result<Self, DataStreamError> {
        match byte {
            1 => Ok(Self::TcpOutgoing),
            other => Err(DataStreamError::UnknownKind(other)),
        }
    }
}

/// Identifies the intercepted connection a data stream carries.
///
/// Written by the operator as the first bytes of every data stream it opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataStreamHeader {
    pub kind: DataStreamKind,
    /// The connection's id in the agent's own numbering, as the agent reported it on the control
    /// stream.
    pub connection_id: u64,
}

impl DataStreamHeader {
    const LEN: usize = 1 + size_of::<u64>();

    fn encode(&self) -> [u8; Self::LEN] {
        let mut buffer = [0_u8; Self::LEN];
        buffer[0] = self.kind as u8;
        buffer[1..].copy_from_slice(&self.connection_id.to_be_bytes());
        buffer
    }

    fn decode(buffer: [u8; Self::LEN]) -> Result<Self, DataStreamError> {
        Ok(Self {
            kind: DataStreamKind::from_byte(buffer[0])?,
            connection_id: u64::from_be_bytes(
                buffer[1..]
                    .try_into()
                    .expect("header remainder is exactly eight bytes"),
            ),
        })
    }
}

/// Opens data streams on an established connection. Held by the operator.
///
/// Handing this out rather than the [`quinn::Connection`] keeps the QUIC types from leaking into
/// callers that only need to carry connections.
#[derive(Clone)]
pub struct DataStreamOpener(quinn::Connection);

impl DataStreamOpener {
    /// Opens a data stream for an intercepted connection.
    ///
    /// Only valid once the negotiated version is at least [`DATA_STREAM_VERSION`], and only for a
    /// connection the agent has already reported on the control stream.
    pub async fn open(&self, header: DataStreamHeader) -> Result<BiStream, DataStreamError> {
        let (mut send, recv) = self.0.open_bi().await?;
        send.write_all(&header.encode())
            .await
            .map_err(io::Error::from)?;

        Ok(tokio::io::join(recv, send))
    }
}

impl fmt::Debug for DataStreamOpener {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("DataStreamOpener")
            .field(&self.0.remote_address())
            .finish()
    }
}

/// Accepts the next data stream and reads the header identifying its connection. Called by the
/// agent.
pub async fn accept_data_stream(
    connection: &quinn::Connection,
) -> Result<(DataStreamHeader, BiStream), DataStreamError> {
    let (send, mut recv) = connection.accept_bi().await?;

    let mut buffer = [0_u8; DataStreamHeader::LEN];
    recv.read_exact(&mut buffer)
        .await
        .map_err(|error| io::Error::other(error.to_string()))?;

    Ok((
        DataStreamHeader::decode(buffer)?,
        tokio::io::join(recv, send),
    ))
}

/// An established control stream, carrying the framed mirrord-protocol message exchange.
///
/// Owns the connection it belongs to, so that holding the control stream keeps the whole QUIC
/// connection - and any other stream on it - alive.
pub struct ControlStream {
    connection: quinn::Connection,
    stream: BiStream,
    version: u16,
}

impl ControlStream {
    /// The connection this stream belongs to, on which further streams can be opened.
    pub fn connection(&self) -> &quinn::Connection {
        &self.connection
    }

    /// A handle for opening data streams on this connection.
    pub fn data_stream_opener(&self) -> DataStreamOpener {
        DataStreamOpener(self.connection.clone())
    }

    /// Lower of the two peers' [`TRANSPORT_VERSION`]s, and therefore the set of stream conventions
    /// both ends can use.
    pub fn version(&self) -> u16 {
        self.version
    }
}

impl fmt::Debug for ControlStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ControlStream")
            .field("peer", &self.connection.remote_address())
            .field("version", &self.version)
            .finish()
    }
}

impl AsyncRead for ControlStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}

impl AsyncWrite for ControlStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

/// Builds the agent side of the QUIC connection.
///
/// `operator_cert_pem` is the PEM-encoded operator certificate from
/// [`OPERATOR_CERT`](mirrord_agent_env::envs::OPERATOR_CERT). Only a peer that can prove
/// possession of its private key is allowed to connect. The agent's own certificate is generated
/// here and discarded when the returned config is dropped.
pub fn server_config(operator_cert_pem: &str) -> Result<quinn::ServerConfig, QuicSetupError> {
    let mut roots = RootCertStore::empty();
    let mut added = 0;
    for cert in CertificateDer::pem_slice_iter(operator_cert_pem.as_bytes()) {
        roots.add(cert.map_err(QuicSetupError::MalformedOperatorCert)?)?;
        added += 1;
    }
    if added == 0 {
        return Err(QuicSetupError::NoOperatorCert);
    }

    let client_verifier = WebPkiClientVerifier::builder(roots.into()).build()?;

    let agent_cert = mirrord_tls_util::generate_cert(AGENT_SERVER_NAME, None, false)?;
    let agent_key = PrivateKeyDer::try_from(agent_cert.signing_key.serialize_der())
        .map_err(|error| QuicSetupError::MalformedAgentKey(error.to_owned()))?;

    let mut tls_config =
        rustls::ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .with_client_cert_verifier(client_verifier)
            .with_single_cert(vec![agent_cert.cert.der().clone()], agent_key)?;
    tls_config.alpn_protocols = vec![ALPN.to_vec()];

    let mut config =
        quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(tls_config)?));
    config.transport_config(Arc::new(transport_config(None)));

    Ok(config)
}

/// Builds the operator side of the QUIC connection.
///
/// `cert_pem` and `key_pem` are the operator's own certificate and private key. The same
/// certificate is handed to agents so that they can pin it, and it is presented here as the client
/// certificate that the agent checks against that pin.
pub fn client_config(cert_pem: &str, key_pem: &str) -> Result<quinn::ClientConfig, QuicSetupError> {
    let chain = CertificateDer::pem_slice_iter(cert_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(QuicSetupError::MalformedOperatorCert)?;
    if chain.is_empty() {
        return Err(QuicSetupError::NoOperatorCert);
    }
    let key = PrivateKeyDer::from_pem_slice(key_pem.as_bytes())
        .map_err(QuicSetupError::MalformedOperatorKey)?;

    let mut tls_config =
        rustls::ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(DangerousNoVerifierServer))
            .with_client_auth_cert(chain, key)?;
    tls_config.alpn_protocols = vec![ALPN.to_vec()];

    let mut config = quinn::ClientConfig::new(Arc::new(QuicClientConfig::try_from(tls_config)?));
    config.transport_config(Arc::new(transport_config(Some(KEEP_ALIVE_INTERVAL))));

    Ok(config)
}

fn transport_config(keep_alive: Option<Duration>) -> quinn::TransportConfig {
    let mut config = quinn::TransportConfig::default();
    config
        .max_idle_timeout(Some(
            MAX_IDLE_TIMEOUT
                .try_into()
                .expect("idle timeout is within QUIC's representable range"),
        ))
        .keep_alive_interval(keep_alive)
        .max_concurrent_bidi_streams(MAX_CONCURRENT_STREAMS.into())
        .max_concurrent_uni_streams(0_u32.into())
        .stream_receive_window(STREAM_RECEIVE_WINDOW.into())
        .receive_window(CONNECTION_RECEIVE_WINDOW.into());
    config
}

/// The operator's side of the QUIC transport.
///
/// One endpoint, and therefore one UDP socket and one driver task, serves connections to every
/// agent. QUIC multiplexes them by connection id rather than by socket, so there is nothing to
/// gain from a socket per agent.
#[derive(Clone)]
pub struct ClientEndpoint {
    endpoint: quinn::Endpoint,
    /// Whether the socket is IPv6 and reaches IPv4 peers through IPv4-mapped addresses.
    dual_stack: bool,
}

impl ClientEndpoint {
    /// Binds the endpoint on an ephemeral port.
    ///
    /// Prefers a dual-stack IPv6 socket so that agents on either address family are reachable,
    /// and falls back to IPv4 if that fails, e.g. because IPv6 is disabled in the cluster.
    pub fn new(config: quinn::ClientConfig) -> std::io::Result<Self> {
        let dual_stack_socket = || -> std::io::Result<std::net::UdpSocket> {
            let socket = socket2::Socket::new(socket2::Domain::IPV6, socket2::Type::DGRAM, None)?;
            socket.set_only_v6(false)?;
            socket.bind(&SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)).into())?;
            socket.set_nonblocking(true)?;
            Ok(socket.into())
        };

        let (socket, dual_stack) = match dual_stack_socket() {
            Ok(socket) => (socket, true),
            Err(error) => {
                tracing::warn!(
                    %error,
                    "Failed to bind a dual-stack QUIC socket, falling back to IPv4.",
                );
                (
                    std::net::UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)))?,
                    false,
                )
            }
        };

        let mut endpoint = quinn::Endpoint::new(
            quinn::EndpointConfig::default(),
            None,
            socket,
            Arc::new(quinn::TokioRuntime),
        )?;
        endpoint.set_default_client_config(config);

        Ok(Self {
            endpoint,
            dual_stack,
        })
    }

    /// Starts a connection to an agent listening at `address`.
    pub fn connect(&self, address: SocketAddr) -> Result<quinn::Connecting, quinn::ConnectError> {
        // A dual-stack socket can only send to an IPv4 peer through its IPv4-mapped form.
        let address = match address {
            SocketAddr::V4(v4) if self.dual_stack => (v4.ip().to_ipv6_mapped(), v4.port()).into(),
            other => other,
        };

        self.endpoint.connect(address, AGENT_SERVER_NAME)
    }

    /// Waits for all connections on this endpoint to be cleanly shut down.
    pub async fn wait_idle(&self) {
        self.endpoint.wait_idle().await
    }
}

impl std::fmt::Debug for ClientEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientEndpoint")
            .field("local_addr", &self.endpoint.local_addr().ok())
            .field("dual_stack", &self.dual_stack)
            .finish()
    }
}

/// Opens the control stream on a freshly established connection and negotiates the transport
/// version. Called by the operator.
///
/// A successful QUIC handshake does not on its own mean the peer accepted us: under TLS 1.3 the
/// client certificate is verified only after the client considers the handshake complete, so a
/// rejected operator sees `connect` succeed and fails here instead. Treat a connection as usable
/// only once this call has returned.
pub async fn open_control_stream(
    connection: &quinn::Connection,
) -> Result<ControlStream, ControlStreamError> {
    let (send, recv) = connection.open_bi().await?;
    let mut stream = tokio::io::join(recv, send);

    ControlHeader::CURRENT.write_to(&mut stream).await?;
    let peer = ControlHeader::read_from(&mut stream).await?;

    Ok(ControlStream {
        connection: connection.clone(),
        stream,
        version: peer.version.min(TRANSPORT_VERSION),
    })
}

/// Accepts the control stream on a freshly established connection and negotiates the transport
/// version. Called by the agent.
pub async fn accept_control_stream(
    connection: &quinn::Connection,
) -> Result<ControlStream, ControlStreamError> {
    let (send, recv) = connection.accept_bi().await?;
    let mut stream = tokio::io::join(recv, send);

    let peer = ControlHeader::read_from(&mut stream).await?;
    ControlHeader::CURRENT.write_to(&mut stream).await?;

    Ok(ControlStream {
        connection: connection.clone(),
        stream,
        version: peer.version.min(TRANSPORT_VERSION),
    })
}

#[cfg(test)]
mod test {
    use std::net::{Ipv4Addr, SocketAddr};

    use mirrord_tls_util::generate_cert;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    fn install_crypto_provider() {
        let _ = rustls::crypto::CryptoProvider::install_default(
            rustls::crypto::aws_lc_rs::default_provider(),
        );
    }

    /// Certificate and key PEM for an operator, in the shape the operator generates them.
    fn operator_pems() -> (String, String) {
        let cert = generate_cert("operator", None, false).unwrap();
        (cert.cert.pem(), cert.signing_key.serialize_pem())
    }

    /// Runs an agent-side endpoint that accepts one connection and echoes the control stream back.
    async fn spawn_agent(operator_cert_pem: String) -> SocketAddr {
        let endpoint = quinn::Endpoint::server(
            server_config(&operator_cert_pem).unwrap(),
            (Ipv4Addr::LOCALHOST, 0).into(),
        )
        .unwrap();
        let addr = endpoint.local_addr().unwrap();

        tokio::spawn(async move {
            let connection = endpoint.accept().await.unwrap().await.unwrap();
            let mut control = accept_control_stream(&connection).await.unwrap();
            assert_eq!(control.version(), TRANSPORT_VERSION);

            let mut buffer = [0_u8; 5];
            control.read_exact(&mut buffer).await.unwrap();
            control.write_all(&buffer).await.unwrap();
            control.flush().await.unwrap();

            // Report back, on each data stream the peer opens, which connection its header claimed
            // the stream was for.
            while let Ok((header, mut stream)) = accept_data_stream(&connection).await {
                tokio::spawn(async move {
                    assert_eq!(header.kind, DataStreamKind::TcpOutgoing);
                    stream
                        .write_all(&header.connection_id.to_be_bytes())
                        .await
                        .unwrap();
                    // Finishes the sending half, so the peer sees the bytes followed by EOF.
                    // Dropping without this would reset the stream and discard them.
                    stream.shutdown().await.unwrap();
                });
            }
        });

        addr
    }

    fn client_endpoint(config: quinn::ClientConfig) -> quinn::Endpoint {
        let mut endpoint = quinn::Endpoint::client((Ipv4Addr::LOCALHOST, 0).into()).unwrap();
        endpoint.set_default_client_config(config);
        endpoint
    }

    /// The operator can establish a control stream and exchange bytes over it.
    #[tokio::test]
    async fn control_stream_round_trip() {
        install_crypto_provider();

        let (cert_pem, key_pem) = operator_pems();
        let addr = spawn_agent(cert_pem.clone()).await;

        let endpoint = client_endpoint(client_config(&cert_pem, &key_pem).unwrap());
        let connection = endpoint
            .connect(addr, AGENT_SERVER_NAME)
            .unwrap()
            .await
            .unwrap();

        let mut control = open_control_stream(&connection).await.unwrap();
        assert_eq!(control.version(), TRANSPORT_VERSION);

        control.write_all(b"hello").await.unwrap();
        control.flush().await.unwrap();
        let mut buffer = [0_u8; 5];
        control.read_exact(&mut buffer).await.unwrap();
        assert_eq!(&buffer, b"hello");
    }

    /// A peer holding some other key cannot use the connection, which is the property the TCP
    /// transport gets from the agent pinning the operator certificate.
    ///
    /// The rejection surfaces on the control stream rather than on `connect`, for the reason
    /// described on [`open_control_stream`].
    #[tokio::test]
    async fn rejects_client_with_other_certificate() {
        install_crypto_provider();

        let (cert_pem, _) = operator_pems();
        let (other_cert_pem, other_key_pem) = operator_pems();
        let addr = spawn_agent(cert_pem).await;

        let endpoint = client_endpoint(client_config(&other_cert_pem, &other_key_pem).unwrap());
        let Ok(connection) = endpoint.connect(addr, AGENT_SERVER_NAME).unwrap().await else {
            return;
        };

        assert!(open_control_stream(&connection).await.is_err());
    }

    /// The endpoint the operator actually uses reaches an agent bound to an IPv4 address, which on
    /// a dual-stack socket only works through the IPv4-mapped form.
    #[tokio::test]
    async fn client_endpoint_reaches_ipv4_agent() {
        install_crypto_provider();

        let (cert_pem, key_pem) = operator_pems();
        let addr = spawn_agent(cert_pem.clone()).await;

        let endpoint = ClientEndpoint::new(client_config(&cert_pem, &key_pem).unwrap()).unwrap();
        let connection = endpoint.connect(addr).unwrap().await.unwrap();

        let mut control = open_control_stream(&connection).await.unwrap();
        control.write_all(b"hello").await.unwrap();
        control.flush().await.unwrap();
        let mut buffer = [0_u8; 5];
        control.read_exact(&mut buffer).await.unwrap();
        assert_eq!(&buffer, b"hello");
    }

    /// Data streams reach the agent carrying the connection they belong to, and stay independent
    /// of each other.
    #[tokio::test]
    async fn data_streams_carry_their_connection() {
        install_crypto_provider();

        let (cert_pem, key_pem) = operator_pems();
        let addr = spawn_agent(cert_pem.clone()).await;

        let endpoint = client_endpoint(client_config(&cert_pem, &key_pem).unwrap());
        let connection = endpoint
            .connect(addr, AGENT_SERVER_NAME)
            .unwrap()
            .await
            .unwrap();

        let mut control = open_control_stream(&connection).await.unwrap();
        control.write_all(b"hello").await.unwrap();
        control.flush().await.unwrap();
        let mut buffer = [0_u8; 5];
        control.read_exact(&mut buffer).await.unwrap();

        let opener = control.data_stream_opener();

        // Opened out of order, to show that a stream is matched by its header rather than by the
        // order the streams were created in.
        for connection_id in [7_u64, 3, 900] {
            let mut stream = opener
                .open(DataStreamHeader {
                    kind: DataStreamKind::TcpOutgoing,
                    connection_id,
                })
                .await
                .unwrap();

            let mut reported = [0_u8; 8];
            stream.read_exact(&mut reported).await.unwrap();
            assert_eq!(u64::from_be_bytes(reported), connection_id);
        }
    }

    #[test]
    fn data_stream_header_round_trip() {
        let header = DataStreamHeader {
            kind: DataStreamKind::TcpOutgoing,
            connection_id: u64::MAX,
        };
        assert_eq!(DataStreamHeader::decode(header.encode()).unwrap(), header);
    }

    #[test]
    fn data_stream_header_rejects_unknown_kind() {
        let mut buffer = DataStreamHeader {
            kind: DataStreamKind::TcpOutgoing,
            connection_id: 1,
        }
        .encode();
        buffer[0] = 0xff;

        assert!(matches!(
            DataStreamHeader::decode(buffer),
            Err(DataStreamError::UnknownKind(0xff))
        ));
    }

    #[test]
    fn control_header_round_trip() {
        let header = ControlHeader { version: 7 };
        assert_eq!(ControlHeader::decode(header.encode()).unwrap(), header);
    }

    #[test]
    fn control_header_rejects_foreign_bytes() {
        let mut buffer = ControlHeader::CURRENT.encode();
        buffer[0] = b'x';
        assert!(matches!(
            ControlHeader::decode(buffer),
            Err(ControlStreamError::BadMagic)
        ));
    }
}
