use std::{fmt, path::PathBuf, sync::Arc};

use mirrord_config::feature::network::incoming::tls_delivery::{
    LocalTlsDelivery, TlsDeliveryProtocol,
};
use mirrord_tls_util::{
    DangerousNoVerifierServer, FromPemError, HasSubjectAlternateNames, ParsePemError,
    best_effort_root_store, parse_cert_chain, parse_key_der, read_cert_chain, read_key_der,
};
use rustls::{
    ClientConfig, RootCertStore,
    pki_types::{CertificateDer, PrivateKeyDer, ServerName},
};
use thiserror::Error;
use tokio::{sync::OnceCell, task::JoinError};
use tokio_rustls::TlsConnector;

/// Errors that can occur when resolving [`LocalTlsSetup`].
#[derive(Debug, Error)]
pub enum LocalTlsSetupError {
    #[error("no good trust root certificate was found")]
    NoGoodRoot,
    #[error("background task panicked")]
    BackgroundTaskPanicked,
    #[error(transparent)]
    FromPemError(#[from] FromPemError),
    #[error("failed to parse the client certificate or its key: {0}")]
    ParsePemError(#[from] ParsePemError),
    #[error("the client certificate and its key were rejected: {0}")]
    ClientAuthRejected(#[from] rustls::Error),
}

impl From<JoinError> for LocalTlsSetupError {
    fn from(_: JoinError) -> Self {
        Self::BackgroundTaskPanicked
    }
}

/// Client certificate presented to the user application's TLS server, for servers that require
/// one (mutual TLS).
pub enum LocalClientAuth {
    /// PEM files on disk, read when the setup is first used.
    Files { cert: PathBuf, key: PathBuf },
    /// PEM data already in memory, e.g. read from a Kubernetes Secret by the mirrord operator.
    Pem { cert: Vec<u8>, key: Vec<u8> },
}

impl LocalClientAuth {
    async fn load(
        &self,
    ) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>), LocalTlsSetupError> {
        match self {
            Self::Files { cert, key } => Ok((
                read_cert_chain(cert.clone()).await?,
                read_key_der(key.clone()).await?,
            )),
            Self::Pem { cert, key } => Ok((
                parse_cert_chain(cert.as_slice())?,
                parse_key_der(key.as_slice())?,
            )),
        }
    }
}

impl fmt::Debug for LocalClientAuth {
    /// Never prints the key material: this ends up in logs.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Files { cert, key } => f
                .debug_struct("Files")
                .field("cert", cert)
                .field("key", key)
                .finish(),
            Self::Pem { cert, key } => f
                .debug_struct("Pem")
                .field("cert_bytes", &cert.len())
                .field("key_bytes", &key.len())
                .finish(),
        }
    }
}

/// Setup for connecting to the user application's server with TLS.
///
/// Resolved lazily with [`LocalTlsSetup::resolve`].
pub struct LocalTlsSetup {
    trust_roots: Option<Vec<PathBuf>>,
    server_cert: Option<PathBuf>,
    server_name: Option<ServerName<'static>>,
    client_auth: Option<LocalClientAuth>,

    resolved: OnceCell<(ClientConfig, Option<ServerName<'static>>)>,
}

impl LocalTlsSetup {
    pub fn new(
        trust_roots: Option<Vec<PathBuf>>,
        server_cert: Option<PathBuf>,
        server_name: Option<ServerName<'static>>,
        client_auth: Option<LocalClientAuth>,
    ) -> Self {
        Self {
            trust_roots,
            server_cert,
            server_name,
            client_auth,
            resolved: OnceCell::new(),
        }
    }

    pub fn from_config(config: LocalTlsDelivery) -> Option<Arc<Self>> {
        match config.protocol {
            TlsDeliveryProtocol::Tcp => None,
            TlsDeliveryProtocol::Tls => {
                let server_name = config.server_name.and_then(|name| {
                    ServerName::try_from(name)
                        .inspect_err(|_| {
                            tracing::error!(
                                "Invalid server name was specified for the local HTTPS delivery. \
                                This should be detected during config verification."
                            )
                        })
                        .ok()
                });

                // Config verification guarantees the cert and the key come together.
                let client_auth = config
                    .client_cert
                    .zip(config.client_key)
                    .map(|(cert, key)| LocalClientAuth::Files { cert, key });

                Some(Arc::new(Self::new(
                    config.trust_roots,
                    config.server_cert,
                    server_name,
                    client_auth,
                )))
            }
        }
    }

    /// Returns a [`TlsConnector`] and an optional [`ServerName`] to use when making the TLS
    /// connection.
    pub async fn get(
        &self,
        alpn_protocol: Option<Vec<u8>>,
    ) -> Result<(TlsConnector, Option<ServerName<'static>>), LocalTlsSetupError> {
        let (mut config, server_name) = self
            .resolved
            .get_or_try_init(|| self.resolve())
            .await?
            .clone();
        config.alpn_protocols.extend(alpn_protocol);

        Ok((TlsConnector::from(Arc::new(config)), server_name))
    }

    async fn resolve(
        &self,
    ) -> Result<(ClientConfig, Option<ServerName<'static>>), LocalTlsSetupError> {
        let mut server_name = self.server_name.clone();

        let builder = if let Some(cert_pem) = self.server_cert.clone() {
            let certs = mirrord_tls_util::read_cert_chain(cert_pem).await?;

            if server_name.is_none() {
                let end_entity = certs
                    .first()
                    .expect("read_cert_chain fails when no certificate is found");

                server_name = end_entity
                    .subject_alternate_names()
                    .inspect_err(|error| {
                        tracing::error!(%error, "Failed to extract Subject Alternate Names from the local server's certificate")
                    })
                    .unwrap_or_default()
                    .into_iter()
                    .next();
            }

            let mut store = RootCertStore::empty();
            for cert in certs {
                let _ = store.add(cert);
            }

            if store.is_empty() {
                return Err(LocalTlsSetupError::NoGoodRoot);
            }

            ClientConfig::builder().with_root_certificates(store)
        } else if let Some(trust_roots) = self.trust_roots.clone() {
            let paths = trust_roots.clone();
            let store = best_effort_root_store(paths).await?;

            if store.is_empty() {
                return Err(LocalTlsSetupError::NoGoodRoot);
            }

            ClientConfig::builder().with_root_certificates(store)
        } else {
            ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(DangerousNoVerifierServer))
        };

        let config = match self.client_auth.as_ref() {
            Some(client_auth) => {
                let (cert_chain, key) = client_auth.load().await?;
                builder.with_client_auth_cert(cert_chain, key)?
            }
            None => builder.with_no_client_auth(),
        };

        Ok((config, server_name))
    }
}

impl fmt::Debug for LocalTlsSetup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalTlsSetup")
            .field("trust_roots", &self.trust_roots)
            .field("server_cert", &self.server_cert)
            .field("server_name", &self.server_name)
            .field("client_auth", &self.client_auth)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use mirrord_tls_util::generate_cert;
    use rustls::{RootCertStore, ServerConfig, server::WebPkiClientVerifier};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    use tokio_rustls::TlsAcceptor;

    use super::*;

    /// Accepts one connection on a server that requires a client certificate signed by the
    /// returned root, and reports whether the handshake succeeded.
    async fn mtls_server() -> (
        std::net::SocketAddr,
        rcgen::CertifiedKey<rcgen::KeyPair>,
        tokio::task::JoinHandle<Result<(), std::io::Error>>,
    ) {
        let root = generate_cert("test.root", None, true).unwrap();
        let server = generate_cert("localhost", Some(&root), false).unwrap();

        let mut roots = RootCertStore::empty();
        roots.add(root.cert.der().clone()).unwrap();
        let verifier = WebPkiClientVerifier::builder(Arc::new(roots))
            .build()
            .unwrap();
        let config = ServerConfig::builder()
            .with_client_cert_verifier(verifier)
            .with_single_cert(
                vec![server.cert.der().clone()],
                PrivateKeyDer::Pkcs8(server.signing_key.serialize_der().into()),
            )
            .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(config));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await?;
            let mut stream = acceptor.accept(stream).await?;
            let mut buf = [0u8; 1];
            stream.read_exact(&mut buf).await?;
            stream.write_all(&buf).await?;
            Ok(())
        });

        (addr, root, task)
    }

    async fn connect(setup: LocalTlsSetup, addr: std::net::SocketAddr) {
        let (connector, server_name) = setup.get(None).await.unwrap();
        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let mut stream = connector
            .connect(server_name.unwrap(), stream)
            .await
            .unwrap();
        // Any error from the server's verification of our certificate surfaces on the first
        // read, not during our side of the handshake.
        let _ = stream.write_all(b"x").await;
        let _ = stream.read_exact(&mut [0u8; 1]).await;
    }

    /// A local server that requires a client certificate (mutual TLS) rejected every stolen
    /// request with `BadCertificate`, because the intproxy always connected anonymously. With
    /// `client_auth`, the intproxy presents the certificate and the server accepts it.
    #[tokio::test]
    async fn client_auth_is_presented_to_an_mtls_server() {
        let (addr, root, server) = mtls_server().await;
        let client = generate_cert("client", Some(&root), false).unwrap();

        let setup = LocalTlsSetup::new(
            None,
            None,
            Some(ServerName::try_from("localhost").unwrap()),
            Some(LocalClientAuth::Pem {
                cert: client.cert.pem().into_bytes(),
                key: client.signing_key.serialize_pem().into_bytes(),
            }),
        );
        connect(setup, addr).await;

        server
            .await
            .unwrap()
            .expect("the server should accept the presented client certificate");
    }

    #[tokio::test]
    async fn anonymous_client_is_rejected_by_an_mtls_server() {
        let (addr, _root, server) = mtls_server().await;

        let setup = LocalTlsSetup::new(
            None,
            None,
            Some(ServerName::try_from("localhost").unwrap()),
            None,
        );
        connect(setup, addr).await;

        server
            .await
            .unwrap()
            .expect_err("the server should reject a client without a certificate");
    }
}
