use std::{
    fmt::{self, Debug},
    io,
    net::IpAddr,
    sync::Arc,
};

use actix_codec::Framed;
use futures::{SinkExt, TryStreamExt};
use mirrord_protocol::{ClientMessage, DaemonCodec, DaemonMessage};
use thiserror::Error;
use tokio::net::TcpStream;
use tokio_rustls::{
    client::TlsStream,
    rustls::{
        pki_types::{DnsName, ServerName},
        ClientConfig, RootCertStore,
    },
    TlsConnector,
};
use x509_parser::{
    certificate::X509Certificate,
    error::{PEMError, X509Error},
    extensions::GeneralName,
    nom, pem,
};

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
    /// For this method to accept the given `certificate_pem`:
    /// 1. The X509 certificate must be located in the *first* PEM block. Only the *first* PEM block
    ///    is inspected.
    /// 2. The X509 certificate must contain exactly one SAN extension.
    /// 3. The SAN extension must contain at least one SAN that is a DNS name or an IP address. This
    ///    requirement comes from [`TlsConnector::connect`] interface.
    #[tracing::instrument(level = "trace", err(Debug))]
    pub fn new(certificate_pem: String) -> Result<Self, TlsSetupError> {
        let (_, pem) = pem::parse_x509_pem(certificate_pem.as_bytes())?;
        let cert = pem.parse_x509()?;
        let server_name = Self::get_san(&cert).ok_or(TlsSetupError::NoSubjectAlternateName)?;

        let mut root_store = RootCertStore::empty();
        root_store.add(pem.contents.into())?;

        let inner = TlsConnector::from(Arc::new(
            ClientConfig::builder()
                .with_root_certificates(root_store)
                .with_no_client_auth(),
        ));

        Ok(Self { inner, server_name })
    }

    /// Retrieves [`ServerName`] from the given certificate.
    /// If the certificate does not contain exactly one SAN extension or the extension does not
    /// contain any SAN that is a DNS name or an IP address, this method returns [`None`].
    fn get_san(cert: &X509Certificate<'_>) -> Option<ServerName<'static>> {
        let extension = match cert.subject_alternative_name() {
            Ok(Some(extension)) => extension,
            Ok(None) => {
                tracing::error!("no SAN extension found");
                return None;
            }
            Err(error) => {
                tracing::error!(%error, "failed to extract SAN extension");
                return None;
            }
        };

        extension
            .value
            .general_names
            .iter()
            .find_map(|general_name| match *general_name {
                GeneralName::DNSName(name) => {
                    let dns_name = DnsName::try_from(name)
                        .inspect_err(|error| {
                            tracing::error!(%error, name, "SAN extension contains an invalid DNS name")
                        })
                        .ok()?
                        .to_owned();

                    Some(ServerName::DnsName(dns_name))
                }

                GeneralName::IPAddress(ip) => {
                    let addr = if let Ok(addr) = <[u8; 4]>::try_from(ip) {
                        IpAddr::from(addr)
                    } else if let Ok(addr) = <[u8; 16]>::try_from(ip) {
                        IpAddr::from(addr)
                    } else {
                        tracing::error!(?ip, "SAN extension contains an invalid IP address");
                        return None;
                    };

                    Some(ServerName::IpAddress(addr.into()))
                }

                _ => None,
            })
    }
}

/// Errors that can occur when creating an [`AgentTlsConnector`].
#[derive(Debug, Error)]
pub(crate) enum TlsSetupError {
    /// We managed to decode the given PEM, but failed to extract the certificate from the decoded
    /// data.
    #[error("failed to extract the X509 certificate from PEM data")]
    Parse(#[from] nom::Err<X509Error>),
    /// We failed to parse the PEM.
    #[error("failed to parse certificate PEM: {0}")]
    Pem(#[from] nom::Err<PEMError>),
    /// The certificate did not contain any SAN we can use when making TLS connections.
    #[error("provided operator certificate has no valid Subject Alternate Name")]
    NoSubjectAlternateName,
    /// We failed to add the certificate to the [`RootCertStore`].
    #[error("rustls failed: {0}")]
    Rustls(#[from] tokio_rustls::rustls::Error),
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
    use std::sync::Arc;

    use futures::StreamExt;
    use mirrord_protocol::ClientCodec;
    use tokio::net::{TcpListener, TcpStream};
    use tokio_rustls::{
        rustls::{pki_types::PrivateKeyDer, ServerConfig},
        TlsAcceptor,
    };

    use super::*;

    /// Verifies that [`AgentTlsConnector`] correctly accepts a
    /// connection from a server using the provided certificate.
    #[tokio::test]
    async fn agent_tls_connector_valid_cert() {
        let cert = rcgen::generate_simple_self_signed(vec!["operator".to_string()]).unwrap();
        let cert_bytes = cert.serialize_der().unwrap();
        let key_bytes = cert.get_key_pair().serialize_der();
        let acceptor = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert_bytes.into()],
                PrivateKeyDer::Pkcs8(key_bytes.into()),
            )
            .map(Arc::new)
            .map(TlsAcceptor::from)
            .unwrap();

        let connector = AgentTlsConnector::new(cert.serialize_pem().unwrap()).unwrap();

        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
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
        let server_cert = rcgen::generate_simple_self_signed(vec!["operator".to_string()]).unwrap();
        let server_cert_bytes = server_cert.serialize_der().unwrap();
        let key_bytes = server_cert.get_key_pair().serialize_der();
        let acceptor = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![server_cert_bytes.into()],
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
                    rcgen::generate_simple_self_signed(vec!["operator".to_string()])
                        .unwrap()
                        .serialize_pem()
                        .unwrap(),
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
