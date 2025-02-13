use std::{fmt, path::PathBuf, sync::Arc};

use mirrord_tls_util::{
    rustls::{self, ClientConfig},
    tokio_rustls::TlsConnector,
    CertChain, DangerousNoVerifier, TlsUtilError,
};
use thiserror::Error;
use tokio::{sync::OnceCell, task::JoinError};
use tracing::Level;

#[derive(Error, Debug)]
pub enum LazyConnectorError {
    #[error("background task panicked")]
    BackgroundTaskPanicked,
    #[error("failed to parse internal proxy certicate chain: {0}")]
    ParseIntproxyCertError(#[from] TlsUtilError),
    #[error("internal proxy certificate chain is invalid: {0}")]
    InvalidIntproxyCertError(#[from] rustls::Error),
}

impl From<JoinError> for LazyConnectorError {
    fn from(_: JoinError) -> Self {
        Self::BackgroundTaskPanicked
    }
}

pub struct LazyConnector {
    cert_and_key: Option<(PathBuf, PathBuf)>,
    connector: OnceCell<TlsConnector>,
}

impl LazyConnector {
    pub fn anonymous() -> Self {
        Self {
            cert_and_key: None,
            connector: OnceCell::new(),
        }
    }

    pub fn authenticated(cert_pem: PathBuf, key_pem: PathBuf) -> Self {
        Self {
            cert_and_key: Some((cert_pem, key_pem)),
            connector: OnceCell::new(),
        }
    }

    pub async fn get(&self) -> Result<&TlsConnector, LazyConnectorError> {
        let connector = self
            .connector
            .get_or_try_init(|| self.build_connector())
            .await?;

        Ok(connector)
    }

    #[tracing::instrument(level = Level::TRACE, err)]
    async fn build_connector(&self) -> Result<TlsConnector, LazyConnectorError> {
        let cert_and_key = self.cert_and_key.clone();

        tokio::task::spawn_blocking(move || {
            let builder = ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(DangerousNoVerifier));

            let config = match cert_and_key {
                Some((cert, key)) => {
                    let chain = CertChain::read(&cert, &key)
                        .map_err(LazyConnectorError::ParseIntproxyCertError)?;
                    let (cert_chain, key_der) = chain.into_chain_and_key();
                    builder
                        .with_client_auth_cert(cert_chain, key_der)
                        .map_err(LazyConnectorError::InvalidIntproxyCertError)?
                }
                None => builder.with_no_client_auth(),
            };

            Ok::<_, LazyConnectorError>(TlsConnector::from(Arc::new(config)))
        })
        .await?
    }
}

impl fmt::Debug for LazyConnector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LazyConnector")
            .field(
                "cert_pem",
                &self.cert_and_key.as_ref().map(|tuple| &tuple.0),
            )
            .field("key_pem", &self.cert_and_key.as_ref().map(|tuple| &tuple.1))
            .field("built", &self.connector.get().is_some())
            .finish()
    }
}
