use std::{fmt, io};

use actix_codec::Framed;
use futures::{SinkExt, TryStreamExt};
use mirrord_protocol::{ClientMessage, DaemonCodec, DaemonMessage};
use mirrord_tls_util::{
    rustls::pki_types::ServerName,
    tokio_rustls::{client::TlsStream, TlsConnector},
    ConnectorClientAuth, ConnectorServerAuth, SingleCertRootStore, TlsConnectorConfig,
    TlsConnectorExt, TlsUtilError,
};
use tokio::net::TcpStream;
use tracing::Level;

use crate::util::ClientId;

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
    /// See [`SingleCertRootStore::read`] docs for the expected certificate format.
    #[tracing::instrument(level = Level::TRACE, err(level = Level::ERROR))]
    pub fn new(certificate_pem: String) -> Result<Self, TlsUtilError> {
        let root_store = SingleCertRootStore::read(certificate_pem.as_bytes())?;
        let server_name = root_store.server_name().clone();

        let config = TlsConnectorConfig {
            server_auth: ConnectorServerAuth::SingleCertRootStore(root_store),
            client_auth: ConnectorClientAuth::Anonymous,
        };

        let inner = TlsConnector::build_from_config(config)?;

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
        rustls, tokio_rustls::TlsAcceptor, AcceptorClientAuth, AsPem, CertWithKey,
        TlsAcceptorConfig, TlsAcceptorExt,
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

        let cert = CertWithKey::new_random_self_signed(["operator".to_string()]).unwrap();
        let cert_pem = cert.cert_chain().first().unwrap().as_pem();
        let config = TlsAcceptorConfig {
            server_auth: cert,
            client_auth: AcceptorClientAuth::Disabled,
            alpn_protocols: Default::default(),
        };
        let acceptor = TlsAcceptor::build_from_config(config).unwrap();

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

        let cert = CertWithKey::new_random_self_signed(["operator".to_string()]).unwrap();
        let config = TlsAcceptorConfig {
            server_auth: cert,
            client_auth: AcceptorClientAuth::Disabled,
            alpn_protocols: Default::default(),
        };
        let acceptor = TlsAcceptor::build_from_config(config).unwrap();

        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::join!(
            async move {
                let connector = AgentTlsConnector::new(
                    CertWithKey::new_random_self_signed(["operator".to_string()])
                        .unwrap()
                        .cert_chain()
                        .first()
                        .unwrap()
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
