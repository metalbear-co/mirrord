use std::{
    fmt::{self, Debug},
    io,
    sync::Arc,
};

use actix_codec::Framed;
use futures::{SinkExt, TryStreamExt};
use mirrord_protocol::{ClientMessage, DaemonMessage, MeasuredDaemonCodec};
use mirrord_protocol_io::{Agent, Connection};
use mirrord_tls_util::{GetSanError, HasSubjectAlternateNames};
use thiserror::Error;
use tokio::net::TcpStream;
use tokio_rustls::{
    TlsConnector,
    client::TlsStream,
    rustls::{ClientConfig, RootCertStore, pki_types::ServerName},
};
use tracing::Level;
use x509_parser::{error::PEMError, nom, pem};

use crate::{metrics::Buffered, util::ClientId};

/// Wrapper over [`TlsConnector`] that can make successful TLS connections only to the server using
/// a predefined certificate.
///
/// Can be used in [`ClientConnection::new`] to secure the incoming TCP connection with TLS.
#[derive(Clone)]
pub struct AgentTlsConnector {
    /// Build to accept only the predefined certificate.
    inner: TlsConnector,
    /// Extracted from the certificate, used in [`TlsConnector::connect`].
    server_name: ServerName<'static>,
}

impl AgentTlsConnector {
    /// Crates a new instance of this connector. The connector will make successful TLS connections
    /// only to the server using the given PEM-encoded certificate.
    ///
    /// For this method to accept the given `certificate_pem`:
    /// 1. The X509 certificate must be located in the *first* PEM block. Only the *first* PEM block
    ///    is inspected.
    /// 2. The X509 certificate must contain exactly one SAN extension.
    /// 3. The SAN extension must contain at least one SAN that is a DNS name or an IP address. This
    ///    requirement comes from [`TlsConnector::connect`] interface.
    #[tracing::instrument(level = Level::TRACE, err(Debug))]
    pub fn new(certificate_pem: String) -> Result<Self, TlsSetupError> {
        let (_, pem) = pem::parse_x509_pem(certificate_pem.as_bytes())?;
        let server_name = pem
            .subject_alternate_names()?
            .into_iter()
            .next()
            .ok_or(TlsSetupError::NoSubjectAlternateName)?;

        let mut root_store = RootCertStore::empty();
        root_store.add(pem.contents.into())?;

        let inner = TlsConnector::from(Arc::new(
            ClientConfig::builder()
                .with_root_certificates(root_store)
                .with_no_client_auth(),
        ));

        Ok(Self { inner, server_name })
    }
}

/// Errors that can occur when creating an [`AgentTlsConnector`].
#[derive(Debug, Error)]
pub(crate) enum TlsSetupError {
    /// We failed to parse the PEM.
    #[error("failed to parse certificate PEM: {0}")]
    PemError(#[from] nom::Err<PEMError>),
    /// The certificate did not contain any SAN we can use when making TLS connections.
    #[error("provided operator certificate has no valid Subject Alternate Name")]
    NoSubjectAlternateName,
    /// We failed to extract the names from the certificate.
    #[error("failed to extract Subject Alternate Names: {0}")]
    GetSanError(#[from] GetSanError),
    /// We failed to add the certificate to the [`RootCertStore`].
    #[error("failed to add the certificate to the root store: {0}")]
    AddToRootStoreError(#[from] tokio_rustls::rustls::Error),
}

/// Wrapper over client's network connection with the agent.
pub struct ClientConnection {
    framed: ConnectionFramed,
    client_id: ClientId,
}

impl ClientConnection {
    /// Wraps the given [`TcpStream`] into this struct.
    /// If an [`AgentTlsConnector`] is given, it is used to first make a TLS connection using the
    /// given [`TcpStream`].
    #[tracing::instrument(level = "trace", skip(tls), fields(use_tls = tls.is_some()), err)]
    pub async fn new(
        stream: TcpStream,
        client_id: u32,
        tls: Option<AgentTlsConnector>,
    ) -> io::Result<Self> {
        // Every message the agent sends a session travels over this one socket, including each
        // chunk relayed from an intercepted connection, so buffering small writes here only adds
        // latency to the session. The client sets this on its end of the same connection. Failing
        // to set it costs latency, not correctness, so it must not fail the connection.
        if let Err(error) = stream.set_nodelay(true) {
            tracing::warn!(%error, "Failed to set TCP_NODELAY on a client connection");
        }

        let framed = match tls {
            Some(connector) => {
                let tls_stream = connector
                    .inner
                    .connect(connector.server_name.clone(), stream)
                    .await?;

                ConnectionFramed::Tls(Box::new(Framed::new(
                    tls_stream,
                    MeasuredDaemonCodec::default(),
                )))
            }
            None => ConnectionFramed::Tcp(Framed::new(stream, MeasuredDaemonCodec::default())),
        };

        Ok(Self { framed, client_id })
    }

    /// Wraps a protocol connection that is already backconnected to the client.
    #[tracing::instrument(level = "trace", skip(connection))]
    pub(crate) fn from_protocol_connection(connection: Connection<Agent>, client_id: u32) -> Self {
        Self {
            framed: ConnectionFramed::ProtocolConnection(connection),
            client_id,
        }
    }

    /// Sends a [`DaemonMessage`] to the client.
    ///
    /// The message's bytes stay counted until this returns. On the [`Framed`] connections that is
    /// when they have been flushed to the client. A protocol-io [`Connection`] returns once the
    /// message is queued for its I/O task, so the bytes it holds in that queue are not counted.
    #[tracing::instrument(level = "trace", err)]
    pub async fn send(&mut self, message: Buffered<DaemonMessage>) -> io::Result<()> {
        let (message, _buffered_bytes) = message.into_parts();

        match &mut self.framed {
            ConnectionFramed::Tcp(framed) => framed.send(message).await?,
            ConnectionFramed::Tls(framed) => framed.send(message).await?,
            ConnectionFramed::ProtocolConnection(connection) => {
                connection.send(message.into_inner()).await;

                if connection.is_closed() {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "protocol-io connection is closed",
                    ));
                }
            }
        }

        Ok(())
    }

    /// Receives a [`ClientMessage`] from the client.
    ///
    /// The message's bytes stay counted in the buffered bytes metric until the returned
    /// [`Buffered`] is dropped, so the caller should keep its guard (see [`Buffered::into_parts`])
    /// while it handles the message.
    #[tracing::instrument(level = "trace", err)]
    pub async fn receive(&mut self) -> io::Result<Option<Buffered<ClientMessage>>> {
        let message = match &mut self.framed {
            ConnectionFramed::Tcp(framed) => framed.try_next().await?.map(Buffered::from_measured),
            ConnectionFramed::Tls(framed) => framed.try_next().await?.map(Buffered::from_measured),
            ConnectionFramed::ProtocolConnection(connection) => {
                connection.recv().await.map(Buffered::new)
            }
        };

        Ok(message)
    }
}

impl fmt::Debug for ClientConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientConnection")
            .field("client_id", &self.client_id)
            .field(
                "uses_tls",
                &matches!(self.framed, ConnectionFramed::Tls(..)),
            )
            .finish()
    }
}

/// Enum wraps whole [`Framed`] instead of just [`TcpStream`]/[`TlsStream`], so we don't have to
/// implement [`AsyncRead`](actix_codec::AsyncRead) and [`AsyncWrite`](actix_codec::AsyncWrite).
/// It also carries protocol-io `Connection<Agent>`
enum ConnectionFramed {
    Tcp(Framed<TcpStream, MeasuredDaemonCodec>),
    Tls(Box<Framed<TlsStream<TcpStream>, MeasuredDaemonCodec>>),
    ProtocolConnection(Connection<Agent>),
}

#[cfg(test)]
mod test {
    use std::{ops::Not, sync::Arc, time::Duration};

    use actix_codec::Encoder;
    use bytes::BytesMut;
    use futures::StreamExt;
    use mirrord_protocol::{
        ClientCodec, FileRequest, FileResponse, Measured,
        file::{ReadFileRequest, ReadFileResponse},
    };
    use tokio::net::{TcpListener, TcpStream};
    use tokio_rustls::{
        TlsAcceptor,
        rustls::{ServerConfig, pki_types::PrivateKeyDer},
    };

    use super::*;
    use crate::metrics::BUFFERED_MESSAGES_BYTES;

    /// Verifies that [`AgentTlsConnector`] correctly accepts a
    /// connection from a server using the provided certificate.
    #[tokio::test]
    async fn agent_tls_connector_valid_cert() {
        let _ = rustls::crypto::CryptoProvider::install_default(
            rustls::crypto::aws_lc_rs::default_provider(),
        );

        let cert = rcgen::generate_simple_self_signed(vec!["operator".to_owned()]).unwrap();
        let cert_bytes = cert.cert.der();
        let key_bytes = cert.signing_key.serialize_der();
        let acceptor = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert_bytes.clone()],
                PrivateKeyDer::Pkcs8(key_bytes.into()),
            )
            .map(Arc::new)
            .map(TlsAcceptor::from)
            .unwrap();

        let connector = AgentTlsConnector::new(cert.cert.pem()).unwrap();

        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::join!(
            async move {
                let stream = TcpStream::connect(addr).await.unwrap();
                let mut connection = ClientConnection::new(stream, 0, Some(connector))
                    .await
                    .unwrap();
                connection
                    .send(Buffered::new(DaemonMessage::Close("it works".into())))
                    .await
                    .unwrap();
            },
            async move {
                let (stream, _) = listener.accept().await.unwrap();
                let connection = acceptor.accept(stream).await.unwrap();
                let mut framed = Framed::new(connection, ClientCodec::default());
                match framed.next().await.unwrap() {
                    Ok(DaemonMessage::Close(msg)) if msg == "it works" => {}
                    other => panic!("unexpected message: {other:?}"),
                }
            },
        );
    }

    /// Verifies that [`AgentTlsConnector`] correctly rejects a
    /// connection from a server using some other certificate.
    #[tokio::test]
    async fn agent_tls_connector_invalid_cert() {
        let _ = rustls::crypto::CryptoProvider::install_default(
            rustls::crypto::aws_lc_rs::default_provider(),
        );

        let server_cert = rcgen::generate_simple_self_signed(vec!["operator".to_owned()]).unwrap();
        let cert_bytes = server_cert.cert.der();
        let key_bytes = server_cert.signing_key.serialize_der();
        let acceptor = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert_bytes.clone()],
                PrivateKeyDer::Pkcs8(key_bytes.into()),
            )
            .map(Arc::new)
            .map(TlsAcceptor::from)
            .unwrap();

        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::join!(
            async move {
                let connector = AgentTlsConnector::new(
                    rcgen::generate_simple_self_signed(vec!["operator".to_owned()])
                        .unwrap()
                        .cert
                        .pem(),
                )
                .unwrap();

                let stream = TcpStream::connect(addr).await.unwrap();
                ClientConnection::new(stream, 0, Some(connector))
                    .await
                    .unwrap_err();
            },
            async move {
                let (stream, _) = listener.accept().await.unwrap();
                acceptor.accept(stream).await.unwrap_err();
            },
        );
    }

    /// Verifies that a message's bytes are counted in `mirrord_agent_buffered_messages_bytes`
    /// for as long as the agent holds them: an outgoing message until it has been flushed to the
    /// client, and an incoming message until its [`Buffered`] value is dropped.
    ///
    /// The gauges are process-wide, so the test uses file messages, which no other test in this
    /// crate counts, and compares against the values read at the start.
    #[tokio::test]
    async fn buffered_message_bytes_counted_until_sent_or_dropped() {
        let daemon_file_bytes = BUFFERED_MESSAGES_BYTES.with_label_values(&["daemon", "file"]);
        let client_file_bytes = BUFFERED_MESSAGES_BYTES.with_label_values(&["client", "file"]);
        let daemon_baseline = daemon_file_bytes.get();
        let client_baseline = client_file_bytes.get();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let stream = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let mut connection = ClientConnection::new(stream, 0, None).await.unwrap();
        let (client_stream, _) = listener.accept().await.unwrap();
        let mut client = Framed::new(client_stream, ClientCodec::default());

        // Larger than the socket buffers, so `send` cannot finish until the client reads.
        let outgoing = DaemonMessage::File(FileResponse::Read(Ok(ReadFileResponse {
            bytes: vec![7; 32 * 1024 * 1024].into(),
            read_amount: 32 * 1024 * 1024,
        })));
        let outgoing_len = Measured::new(outgoing.clone()).encoded_len();
        let outgoing_buffered = Buffered::new(outgoing.clone());
        let send = tokio::spawn(async move {
            connection.send(outgoing_buffered).await.unwrap();
            connection
        });

        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            send.is_finished().not(),
            "send finished before the client read"
        );
        assert_eq!(
            daemon_file_bytes.get() - daemon_baseline,
            i64::try_from(outgoing_len).unwrap()
        );

        assert_eq!(client.next().await.unwrap().unwrap(), outgoing);
        let mut connection = send.await.unwrap();
        assert_eq!(daemon_file_bytes.get(), daemon_baseline);

        let incoming = ClientMessage::FileRequest(FileRequest::Read(ReadFileRequest {
            remote_fd: 3,
            buffer_size: 4096,
        }));
        let mut incoming_frame = BytesMut::new();
        ClientCodec::default()
            .encode(incoming.clone(), &mut incoming_frame)
            .unwrap();
        client.send(incoming.clone()).await.unwrap();

        let received = connection.receive().await.unwrap().unwrap();
        assert_eq!(*received, incoming);
        assert_eq!(
            client_file_bytes.get() - client_baseline,
            i64::try_from(incoming_frame.len()).unwrap()
        );

        drop(received);
        assert_eq!(client_file_bytes.get(), client_baseline);
    }
}
