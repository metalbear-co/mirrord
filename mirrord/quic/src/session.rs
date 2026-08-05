//! QUIC transport for the connection between the mirrord CLI and the Operator.
//!
//! # Why QUIC
//!
//! The session connection normally reaches the operator through the Kubernetes API server, which
//! proxies it as a websocket. That path is convenient - it authenticates the user, it needs no
//! network setup, and it works from anywhere a `kubectl` works - but every byte of the session
//! crosses a component whose job is serving API requests, and which buffers, rate limits and
//! disconnects accordingly. Dialing the operator directly takes the API server out of the data
//! path.
//!
//! # Streams
//!
//! One bidirectional stream per session, opened by the CLI. After the preamble below it carries
//! the mirrord-protocol message exchange framed exactly as the websocket carries it, so both ends
//! reuse their existing codecs. Nothing else is opened on the connection.
//!
//! # Trust model
//!
//! Taking the API server out of the path also takes out the thing that authenticated the user, so
//! neither end can be trusted on its own say-so. Both directions are solved by bootstrapping from
//! the API server path, which is still used to set the session up:
//!
//! * **The operator trusts the CLI** because the CLI presents a ticket that the operator issued
//!   over the API server, to a request the API server had already authenticated. The ticket is
//!   single use, short lived, and carries the identity it was issued to, so nothing about who the
//!   caller is travels over QUIC where it could be forged.
//! * **The CLI trusts the operator** because it pins the exact certificate that the same API server
//!   response handed it. The operator's certificate is usually self-signed and issued for
//!   in-cluster service names, so neither a public root nor hostname verification would say
//!   anything useful about it; byte equality against a certificate fetched over an authenticated
//!   channel is a stronger statement than either.
//!
//! Because the certificate is pinned by value, whatever terminates the advertised address must
//! pass UDP through rather than terminate TLS itself.

use std::{fmt, io, sync::Arc};

use rustls::{
    DigitallySignedStruct, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::CryptoProvider,
    pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime},
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};

use crate::{BiStream, KEEP_ALIVE_INTERVAL, OwnedStream, QuicSetupError, SessionStreamError};

/// ALPN protocol negotiated on every CLI to operator QUIC connection.
///
/// Distinct from the operator to agent ALPN so that pointing one at the other fails during the
/// handshake rather than as a confusing preamble mismatch afterwards.
pub const SESSION_ALPN: &[u8] = b"mirrord-session/1";

/// Highest version of the session stream conventions this build understands.
///
/// Both ends exchange this in the preamble and continue with the lower of the two.
pub const SESSION_VERSION: u16 = 1;

/// Identifies the session stream, so that a peer speaking something else on this port fails
/// immediately and legibly instead of as a protocol decode error further down.
const SESSION_MAGIC: [u8; 6] = *b"mrdses";

/// Server name the CLI passes to [`connect`](quinn::Endpoint::connect).
///
/// The operator's certificate is pinned by value rather than checked against a name, so this never
/// has to match anything. It is only ever seen in logs and in TLS-level errors.
pub const OPERATOR_SERVER_NAME: &str = "mirrord-operator";

/// Largest preamble either end will read.
///
/// The preamble length arrives before the preamble itself, so a peer could otherwise ask either
/// end to allocate an arbitrary amount before it has proven anything at all. The request carries a
/// ticket, a target and the connect parameters, all of which are far below this.
pub const MAX_PREAMBLE_LEN: u32 = 64 * 1024;

/// Whether the operator accepted the session, sent as a single byte so an older CLI reading a
/// status it does not know still knows the session did not start.
const STATUS_ACCEPTED: u8 = 0;

/// Whether the operator refused the session. Any status other than [`STATUS_ACCEPTED`] means
/// refused, so a CLI that meets a status a later operator invented still fails closed.
const STATUS_REFUSED: u8 = 1;

/// An established session stream, carrying the framed mirrord-protocol message exchange.
pub struct SessionStream {
    stream: OwnedStream,
    version: u16,
}

impl SessionStream {
    /// Lower of the two peers' [`SESSION_VERSION`]s, and therefore the set of conventions both
    /// ends can use.
    pub fn version(&self) -> u16 {
        self.version
    }
}

impl fmt::Debug for SessionStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionStream")
            .field("peer", &self.stream.connection().remote_address())
            .field("version", &self.version)
            .finish()
    }
}

impl AsyncRead for SessionStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}

impl AsyncWrite for SessionStream {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        std::pin::Pin::new(&mut self.stream).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.stream).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

/// Reads the magic and returns the version the peer announced.
async fn read_magic(stream: &mut BiStream) -> Result<u16, SessionStreamError> {
    let mut head = [0_u8; SESSION_MAGIC.len() + size_of::<u16>()];
    stream.read_exact(&mut head).await?;

    let (magic, version) = head.split_at(SESSION_MAGIC.len());
    if magic != SESSION_MAGIC {
        return Err(SessionStreamError::BadMagic);
    }

    Ok(u16::from_be_bytes(
        version.try_into().expect("split at the version offset"),
    ))
}

/// Writes the magic and this build's [`SESSION_VERSION`].
async fn write_magic(stream: &mut BiStream) -> Result<(), SessionStreamError> {
    stream.write_all(&SESSION_MAGIC).await?;
    stream.write_all(&SESSION_VERSION.to_be_bytes()).await?;

    Ok(())
}

/// Reads a length-prefixed payload, refusing an oversized one before allocating for it.
async fn read_payload(stream: &mut BiStream) -> Result<Vec<u8>, SessionStreamError> {
    let length = stream.read_u32().await?;
    if length > MAX_PREAMBLE_LEN {
        return Err(SessionStreamError::OversizedPreamble(length));
    }

    let mut payload = vec![0_u8; length as usize];
    stream.read_exact(&mut payload).await?;

    Ok(payload)
}

/// Writes a length-prefixed payload and flushes, which is also what makes a freshly opened stream
/// visible to the peer.
async fn write_payload(stream: &mut BiStream, payload: &[u8]) -> Result<(), SessionStreamError> {
    let length = u32::try_from(payload.len())
        .map_err(|_| SessionStreamError::OversizedPreamble(u32::MAX))?;
    if length > MAX_PREAMBLE_LEN {
        return Err(SessionStreamError::OversizedPreamble(length));
    }

    stream.write_all(&length.to_be_bytes()).await?;
    stream.write_all(payload).await?;
    stream.flush().await?;

    Ok(())
}

/// Opens the session stream on a freshly established connection, sends `request`, and waits for
/// the operator's verdict. Called by the CLI.
///
/// `request` is opaque here: this crate carries it, and the CLI and operator agree on what is in
/// it. Returning [`SessionStreamError::Refused`] means the operator understood the request and
/// declined it, and carries a reason meant for the user; every other error means the session never
/// got that far.
pub async fn open_session_stream(
    connection: &quinn::Connection,
    request: &[u8],
) -> Result<SessionStream, SessionStreamError> {
    let (send, recv) = connection.open_bi().await?;
    let mut stream = tokio::io::join(recv, send);

    write_magic(&mut stream).await?;
    write_payload(&mut stream, request).await?;

    let peer_version = read_magic(&mut stream).await?;
    let status = stream.read_u8().await?;
    let message = read_payload(&mut stream).await?;

    if status != STATUS_ACCEPTED {
        return Err(SessionStreamError::Refused(
            String::from_utf8_lossy(&message).into_owned(),
        ));
    }

    Ok(SessionStream {
        stream: OwnedStream::new(connection, stream),
        version: peer_version.min(SESSION_VERSION),
    })
}

/// Binds a local socket, dials the operator at `address`, and opens the session stream on it.
///
/// The whole dial in one call so that callers need nothing from `quinn` itself. The endpoint is
/// dropped on the way out; quinn keeps its driver alive while the connection is, so the returned
/// stream is all there is to hold onto.
pub async fn connect(
    config: quinn::ClientConfig,
    address: std::net::SocketAddr,
    request: &[u8],
) -> Result<SessionStream, SessionStreamError> {
    let endpoint =
        crate::ClientEndpoint::new_for_session(config).map_err(SessionStreamError::Bind)?;
    let connection = endpoint.connect(address)?.await?;

    open_session_stream(&connection, request).await
}

/// A session the CLI has asked for, which the operator has not yet accepted or refused.
///
/// Deciding takes the whole connect flow - license, policy, Kubernetes permissions, acquiring the
/// session, starting an agent - so the request is read and answered as two separate steps, and the
/// stream is held in between.
pub struct PendingSession {
    connection: quinn::Connection,
    stream: BiStream,
    version: u16,
}

impl PendingSession {
    /// Lower of the two peers' [`SESSION_VERSION`]s.
    pub fn version(&self) -> u16 {
        self.version
    }

    /// The address the request came from, for logging.
    pub fn peer(&self) -> std::net::SocketAddr {
        self.connection.remote_address()
    }

    /// A handle to the underlying connection, which survives this session being accepted.
    pub fn connection(&self) -> SessionConnection {
        SessionConnection(self.connection.clone())
    }

    /// Tells the CLI the session is starting, and hands back the stream it will run on.
    pub async fn accept(mut self) -> Result<SessionStream, SessionStreamError> {
        write_magic(&mut self.stream).await?;
        self.stream.write_u8(STATUS_ACCEPTED).await?;
        write_payload(&mut self.stream, &[]).await?;

        Ok(SessionStream {
            stream: OwnedStream::new(&self.connection, self.stream),
            version: self.version,
        })
    }

    /// Tells the CLI why the session is not starting.
    ///
    /// Sent on the stream rather than as a connection close so that the reason survives as text
    /// the CLI can show, instead of becoming a QUIC error code.
    pub async fn refuse(mut self, reason: &str) -> Result<(), SessionStreamError> {
        // Any status other than STATUS_ACCEPTED means refused; the reason is the useful part.
        write_magic(&mut self.stream).await?;
        self.stream.write_u8(STATUS_REFUSED).await?;
        write_payload(&mut self.stream, reason.as_bytes()).await?;
        self.stream.shutdown().await?;

        Ok(())
    }
}

impl fmt::Debug for PendingSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PendingSession")
            .field("peer", &self.connection.remote_address())
            .field("version", &self.version)
            .finish()
    }
}

/// Accepts the session stream on a freshly established connection and reads the CLI's request.
/// Called by the operator.
pub async fn accept_session_stream(
    connection: &quinn::Connection,
) -> Result<(Vec<u8>, PendingSession), SessionStreamError> {
    let (send, recv) = connection.accept_bi().await?;
    let mut stream = tokio::io::join(recv, send);

    let peer_version = read_magic(&mut stream).await?;
    let request = read_payload(&mut stream).await?;

    Ok((
        request,
        PendingSession {
            connection: connection.clone(),
            stream,
            version: peer_version.min(SESSION_VERSION),
        },
    ))
}

/// Builds the operator side of the QUIC configuration.
///
/// `chain` and `key` are the operator's own serving certificate and private key - the same ones it
/// serves its API with, so that the certificate a CLI pins from the API response is the one it
/// meets here. Clients are not asked for a certificate; the ticket in the request is what
/// authenticates them.
pub fn server_config(
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> Result<quinn::ServerConfig, QuicSetupError> {
    if chain.is_empty() {
        return Err(QuicSetupError::NoOperatorCert);
    }

    let mut tls_config =
        rustls::ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .with_no_client_auth()
            .with_single_cert(chain, key)?;
    tls_config.alpn_protocols = vec![SESSION_ALPN.to_vec()];

    let mut config = quinn::ServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(tls_config)?,
    ));
    config.transport_config(Arc::new(crate::transport_config(None)));

    Ok(config)
}

/// The operator's side of the QUIC transport: one UDP socket accepting session connections.
///
/// Wraps quinn rather than exposing it so that the operator, which otherwise has no QUIC in it,
/// does not take on the dependency for the sake of four calls.
pub struct SessionListener(quinn::Endpoint);

impl SessionListener {
    /// Binds the listener, serving `chain` as the operator's certificate.
    pub fn bind(
        address: std::net::SocketAddr,
        chain: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
    ) -> Result<Self, QuicSetupError> {
        let config = server_config(chain, key)?;

        quinn::Endpoint::server(config, address)
            .map(Self)
            .map_err(QuicSetupError::Bind)
    }

    pub fn local_addr(&self) -> Option<std::net::SocketAddr> {
        self.0.local_addr().ok()
    }

    /// Waits for the next connection. `None` once the listener has been closed.
    pub async fn accept(&self) -> Option<IncomingSession> {
        self.0.accept().await.map(IncomingSession)
    }

    /// Refuses further connections and tells current peers why, rather than leaving them to time
    /// out.
    pub fn close(&self, reason: &[u8]) {
        self.0.close(0_u32.into(), reason);
    }
}

/// A connection that has arrived but whose handshake has not completed.
pub struct IncomingSession(quinn::Incoming);

impl IncomingSession {
    pub fn peer(&self) -> std::net::SocketAddr {
        self.0.remote_address()
    }

    /// Completes the handshake and reads the request the client opened with.
    pub async fn accept(self) -> Result<(Vec<u8>, PendingSession), SessionStreamError> {
        let connection = self.0.await?;

        accept_session_stream(&connection).await
    }
}

/// A handle to the connection a session arrived on, which outlives the stream itself.
///
/// Needed because a session can be refused after its stream has already been handed off, at which
/// point the only way left to say so is to close the connection.
#[derive(Clone)]
pub struct SessionConnection(quinn::Connection);

impl SessionConnection {
    pub fn peer(&self) -> std::net::SocketAddr {
        self.0.remote_address()
    }

    /// Closes the connection, carrying a code and a reason the peer can read.
    pub fn close(&self, code: u32, reason: &[u8]) {
        self.0.close(code.into(), reason);
    }
}

impl fmt::Debug for SessionConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("SessionConnection")
            .field(&self.0.remote_address())
            .finish()
    }
}

/// Builds the CLI side of the QUIC connection.
///
/// `operator_cert_der` is the operator's certificate exactly as it came back over the Kubernetes
/// API server, and is the only certificate this connection will accept. Taken as raw DER so that
/// callers need nothing from `rustls` either.
pub fn client_config(operator_cert_der: Vec<u8>) -> Result<quinn::ClientConfig, QuicSetupError> {
    let operator_cert = CertificateDer::from(operator_cert_der);

    let builder = rustls::ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13]);
    // Taken from the builder rather than from `CryptoProvider::get_default`, which is only set
    // once something has installed a process-wide default. The builder falls back to the one the
    // crate features selected, so this works in a process that never installed one.
    let provider = builder.crypto_provider().clone();

    let mut tls_config = builder
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedServerCert {
            expected: operator_cert,
            provider,
        }))
        .with_no_client_auth();
    tls_config.alpn_protocols = vec![SESSION_ALPN.to_vec()];

    let mut config = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(tls_config)?,
    ));
    config.transport_config(Arc::new(crate::transport_config(Some(KEEP_ALIVE_INTERVAL))));

    Ok(config)
}

/// Accepts one specific certificate, by value, and nothing else.
///
/// Neither the name nor the validity period is checked. Both exist to answer "is this the peer I
/// meant, and is this certificate still one it should be using", and a certificate fetched from
/// that same peer over an authenticated channel moments earlier answers both more directly.
#[derive(Debug)]
struct PinnedServerCert {
    expected: CertificateDer<'static>,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for PinnedServerCert {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if *end_entity == self.expected {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::ApplicationVerificationFailure,
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        // QUIC is TLS 1.3 only, so reaching this means the configuration was built wrong.
        Err(rustls::Error::PeerIncompatible(
            rustls::PeerIncompatible::Tls12NotOffered,
        ))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod test {
    use std::net::{Ipv4Addr, SocketAddr};

    use mirrord_tls_util::generate_cert;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;
    use crate::ClientEndpoint;

    fn install_crypto_provider() {
        let _ = CryptoProvider::install_default(rustls::crypto::aws_lc_rs::default_provider());
    }

    /// A serving certificate in the shape the operator makes one: self-signed, for in-cluster
    /// service names that a CLI dialing an external address could never match.
    fn operator_cert() -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
        let cert = generate_cert("mirrord-operator.mirrord.svc", None, false).unwrap();
        let key = PrivateKeyDer::try_from(cert.signing_key.serialize_der()).unwrap();
        (cert.cert.der().clone(), key)
    }

    /// Runs an operator-side endpoint that accepts one session and either serves or refuses it.
    ///
    /// When it serves, it echoes the stream back so a test can prove the session bytes flow. The
    /// request comes back over a channel rather than as the task's result, because the task has to
    /// outlive the client: dropping the connection closes it and discards anything the client has
    /// not read yet.
    async fn spawn_operator(
        chain: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
        refuse_with: Option<&'static str>,
    ) -> (SocketAddr, tokio::sync::oneshot::Receiver<Vec<u8>>) {
        let endpoint = quinn::Endpoint::server(
            server_config(chain, key).unwrap(),
            (Ipv4Addr::LOCALHOST, 0).into(),
        )
        .unwrap();
        let address = endpoint.local_addr().unwrap();
        let (request_tx, request_rx) = tokio::sync::oneshot::channel();

        tokio::spawn(async move {
            let connection = endpoint.accept().await.unwrap().await.unwrap();
            let (request, pending) = accept_session_stream(&connection).await.unwrap();
            let _ = request_tx.send(request);

            match refuse_with {
                Some(reason) => {
                    pending.refuse(reason).await.unwrap();
                }
                None => {
                    let mut stream = pending.accept().await.unwrap();
                    let mut echoed = [0_u8; 5];
                    stream.read_exact(&mut echoed).await.unwrap();
                    stream.write_all(&echoed).await.unwrap();
                    stream.flush().await.unwrap();
                }
            }

            connection.closed().await;
        });

        (address, request_rx)
    }

    /// The happy path: the CLI pins the certificate it was handed, the operator accepts, and the
    /// request arrives intact along with a stream that carries bytes both ways.
    #[tokio::test]
    async fn session_stream_round_trip() {
        install_crypto_provider();

        let (cert, key) = operator_cert();
        let (address, request) = spawn_operator(vec![cert.clone()], key, None).await;

        let endpoint =
            ClientEndpoint::new_for_session(client_config(cert.to_vec()).unwrap()).unwrap();
        let connection = endpoint.connect(address).unwrap().await.unwrap();
        let mut stream = open_session_stream(&connection, b"ticket-and-target")
            .await
            .unwrap();

        assert_eq!(stream.version(), SESSION_VERSION);

        stream.write_all(b"hello").await.unwrap();
        stream.flush().await.unwrap();
        let mut echoed = [0_u8; 5];
        stream.read_exact(&mut echoed).await.unwrap();
        assert_eq!(&echoed, b"hello");

        assert_eq!(request.await.unwrap(), b"ticket-and-target");
    }

    /// A refusal has to reach the user as text. The CLI must be able to tell "the operator said
    /// no, and here is why" apart from "the connection broke".
    #[tokio::test]
    async fn refusal_carries_its_reason() {
        install_crypto_provider();

        let (cert, key) = operator_cert();
        let (address, _request) =
            spawn_operator(vec![cert.clone()], key, Some("ticket expired")).await;

        let endpoint =
            ClientEndpoint::new_for_session(client_config(cert.to_vec()).unwrap()).unwrap();
        let connection = endpoint.connect(address).unwrap().await.unwrap();

        let error = open_session_stream(&connection, b"ticket-and-target")
            .await
            .expect_err("the operator refused this session");

        assert!(
            matches!(&error, SessionStreamError::Refused(reason) if reason == "ticket expired"),
            "expected a refusal carrying its reason, got {error:?}",
        );
    }

    /// An operator presenting any other certificate must be rejected, even though that certificate
    /// is perfectly valid and self-signed the exact same way. This is the whole point of pinning:
    /// there is no CA in the picture that could vouch for a substitute.
    #[tokio::test]
    async fn rejects_operator_with_other_certificate() {
        install_crypto_provider();

        let (served_cert, served_key) = operator_cert();
        let (pinned_cert, _) = operator_cert();
        let (address, _request) = spawn_operator(vec![served_cert], served_key, None).await;

        let endpoint =
            ClientEndpoint::new_for_session(client_config(pinned_cert.to_vec()).unwrap()).unwrap();
        let result = endpoint.connect(address).unwrap().await;

        assert!(
            result.is_err(),
            "connecting to an operator serving an unpinned certificate should fail",
        );
    }

    /// A peer that announces a huge preamble must be refused before anything is allocated for it,
    /// since the length arrives before any ticket has been checked.
    #[tokio::test]
    async fn rejects_oversized_preamble() {
        install_crypto_provider();

        let (cert, key) = operator_cert();
        let endpoint = quinn::Endpoint::server(
            server_config(vec![cert.clone()], key).unwrap(),
            (Ipv4Addr::LOCALHOST, 0).into(),
        )
        .unwrap();
        let address = endpoint.local_addr().unwrap();

        let operator = tokio::spawn(async move {
            let connection = endpoint.accept().await.unwrap().await.unwrap();
            accept_session_stream(&connection).await
        });

        let client =
            ClientEndpoint::new_for_session(client_config(cert.to_vec()).unwrap()).unwrap();
        let connection = client.connect(address).unwrap().await.unwrap();
        let (mut send, _recv) = connection.open_bi().await.unwrap();
        send.write_all(&SESSION_MAGIC).await.unwrap();
        send.write_all(&SESSION_VERSION.to_be_bytes())
            .await
            .unwrap();
        send.write_all(&(MAX_PREAMBLE_LEN + 1).to_be_bytes())
            .await
            .unwrap();
        send.flush().await.unwrap();

        let error = operator
            .await
            .unwrap()
            .expect_err("an oversized preamble should be refused");
        assert!(
            matches!(error, SessionStreamError::OversizedPreamble(..)),
            "expected an oversized preamble error, got {error:?}",
        );
    }

    /// Something that is not this transport at all must fail as a legible magic mismatch rather
    /// than as a decode error further down.
    #[tokio::test]
    async fn rejects_foreign_preamble() {
        install_crypto_provider();

        let (cert, key) = operator_cert();
        let endpoint = quinn::Endpoint::server(
            server_config(vec![cert.clone()], key).unwrap(),
            (Ipv4Addr::LOCALHOST, 0).into(),
        )
        .unwrap();
        let address = endpoint.local_addr().unwrap();

        let operator = tokio::spawn(async move {
            let connection = endpoint.accept().await.unwrap().await.unwrap();
            accept_session_stream(&connection).await
        });

        let client =
            ClientEndpoint::new_for_session(client_config(cert.to_vec()).unwrap()).unwrap();
        let connection = client.connect(address).unwrap().await.unwrap();
        let (mut send, _recv) = connection.open_bi().await.unwrap();
        send.write_all(b"GET / HTTP/1.1\r\n").await.unwrap();
        send.flush().await.unwrap();

        let error = operator
            .await
            .unwrap()
            .expect_err("a foreign preamble should be refused");
        assert!(
            matches!(error, SessionStreamError::BadMagic),
            "expected a magic mismatch, got {error:?}",
        );
    }
}
