use std::{fmt, io, sync::Arc};

use actix_codec::Framed;
use futures::{SinkExt, TryStreamExt};
use mirrord_protocol::{ClientMessage, DaemonCodec, DaemonMessage};
use mirrord_tls_util::{
    rustls::{self, pki_types::ServerName, ClientConfig, RootCertStore},
    tokio_rustls::{client::TlsStream, TlsConnector},
    CertWithServerName, TlsUtilError,
};
use thiserror::Error;
use tokio::net::TcpStream;
use tracing::Level;

use crate::util::ClientId;

#[derive(Error, Debug)]
pub enum TlsSetupError {
    #[error("failed to read the operator certificate: {0}")]
    ParseOperatorCertError(#[source] TlsUtilError),
    #[error("operator certificate is invalid: {0}")]
    InvalidOperatorCertError(#[source] rustls::Error),
}

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
    /// See [`CertWithServerName::read`] docs for the expected certificate format.
    #[tracing::instrument(level = Level::TRACE, err(level = Level::ERROR))]
    pub fn new(certificate_pem: String) -> Result<Self, TlsSetupError> {
        let cert = CertWithServerName::parse(certificate_pem.as_bytes())
            .map_err(TlsSetupError::ParseOperatorCertError)?;
        let server_name = cert.server_name().clone();

        let mut root_store = RootCertStore::empty();
        root_store
            .add(cert.into())
            .map_err(TlsSetupError::InvalidOperatorCertError)?;

        let client_config = ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();
        let inner = TlsConnector::from(Arc::new(client_config));

        Ok(Self { inner, server_name })
    }
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
        let framed = match tls {
            Some(connector) => {
                let tls_stream = connector
                    .inner
                    .connect(connector.server_name.clone(), stream)
                    .await?;

                ConnectionFramed::Tls(Framed::new(tls_stream, DaemonCodec::default()))
            }
            None => ConnectionFramed::Tcp(Framed::new(stream, DaemonCodec::default())),
        };

        Ok(Self { framed, client_id })
    }

    /// Sends a [`DaemonMessage`] to the client.
    #[tracing::instrument(level = "trace", err)]
    pub async fn send(&mut self, message: DaemonMessage) -> io::Result<()> {
        match &mut self.framed {
            ConnectionFramed::Tcp(framed) => framed.send(message).await?,
            ConnectionFramed::Tls(framed) => framed.send(message).await?,
        }

        Ok(())
    }

    /// Receives a [`ClientMessage`] from the client.
    #[tracing::instrument(level = "trace", err)]
    pub async fn receive(&mut self) -> io::Result<Option<ClientMessage>> {
        match &mut self.framed {
            ConnectionFramed::Tcp(framed) => framed.try_next().await,
            ConnectionFramed::Tls(framed) => framed.try_next().await,
        }
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
enum ConnectionFramed {
    Tcp(Framed<TcpStream, DaemonCodec>),
    Tls(Framed<TlsStream<TcpStream>, DaemonCodec>),
}

#[cfg(test)]
mod test {
    use std::sync::Once;

    use futures::StreamExt;
    use mirrord_protocol::ClientCodec;
    use mirrord_tls_util::{
        rustls::{self, ServerConfig},
        tokio_rustls::TlsAcceptor,
        AsPem, CertChain, RandomCert,
    };
    use tokio::net::{TcpListener, TcpStream};

    use super::*;

    static CRYPTO_PROVIDER: Once = Once::new();

    /// Verifies that [`AgentTlsConnector`] correctly accepts a
    /// connection from a server using the provided certificate.
    #[tokio::test]
    async fn agent_tls_connector_valid_cert() {
        CRYPTO_PROVIDER.call_once(|| {
            rustls::crypto::CryptoProvider::install_default(
                rustls::crypto::aws_lc_rs::default_provider(),
            )
            .expect("Failed to install crypto provider")
        });

        let cert = RandomCert::generate(vec!["operator".to_string()]).unwrap();
        let cert_pem = cert.cert().as_pem();

        let (cert_chain, key_der) = CertChain::from(cert).into_chain_and_key();
        let config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(cert_chain, key_der)
            .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(config));

        let connector = AgentTlsConnector::new(cert_pem).unwrap();

        let listener: TcpListener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::join!(
            async move {
                let stream = TcpStream::connect(addr).await.unwrap();
                let mut connection = ClientConnection::new(stream, 0, Some(connector))
                    .await
                    .unwrap();
                connection
                    .send(DaemonMessage::Close("it works".into()))
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
        CRYPTO_PROVIDER.call_once(|| {
            rustls::crypto::CryptoProvider::install_default(
                rustls::crypto::aws_lc_rs::default_provider(),
            )
            .expect("Failed to install crypto provider")
        });

        let cert = RandomCert::generate(vec!["operator".to_string()]).unwrap();
        let (cert_chain, key_der) = CertChain::from(cert).into_chain_and_key();
        let config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(cert_chain, key_der)
            .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(config));

        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::join!(
            async move {
                let connector = AgentTlsConnector::new(
                    RandomCert::generate(vec!["operator".to_string()])
                        .unwrap()
                        .cert()
                        .as_pem(),
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
}
