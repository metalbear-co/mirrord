use std::{fmt, path::PathBuf, sync::Arc};

use mirrord_tls_util::{
    best_effort_root_store, DangerousNoVerifierServer, FromPemError, HasSubjectAlternateNames,
};
use rustls::{pki_types::ServerName, ClientConfig, RootCertStore};
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
}

impl From<JoinError> for LocalTlsSetupError {
    fn from(_: JoinError) -> Self {
        Self::BackgroundTaskPanicked
    }
}

/// Setup for connecting to the user application's server with TLS.
///
/// Resolved lazily with [`LocalTlsSetup::resolve`].
pub struct LocalTlsSetup {
    trust_roots: Option<Vec<PathBuf>>,
    server_cert: Option<PathBuf>,
    server_name: Option<ServerName<'static>>,

    resolved: OnceCell<(ClientConfig, Option<ServerName<'static>>)>,
}

impl LocalTlsSetup {
    pub fn new(
        trust_roots: Option<Vec<PathBuf>>,
        server_cert: Option<PathBuf>,
        server_name: Option<ServerName<'static>>,
    ) -> Self {
        Self {
            trust_roots,
            server_cert,
            server_name,
            resolved: OnceCell::new(),
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

        Ok((builder.with_no_client_auth(), server_name))
    }
}

impl fmt::Debug for LocalTlsSetup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalTlsSetup")
            .field("trust_roots", &self.trust_roots)
            .field("server_cert", &self.server_cert)
            .field("server_name", &self.server_name)
            .finish()
    }
}
