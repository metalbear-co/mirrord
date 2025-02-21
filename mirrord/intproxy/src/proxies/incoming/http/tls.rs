use std::{path::PathBuf, sync::Arc};

use mirrord_tls_util::{best_effort_root_store, DangerousNoVerifierServer};
use rustls::{pki_types::ServerName, ClientConfig};
use thiserror::Error;
use tokio::{sync::OnceCell, task::JoinError};
use tokio_rustls::TlsConnector;

#[derive(Debug, Error)]
pub enum LocalTlsSetupError {
    #[error("no good trust root certificate was found")]
    NoGoodRoot,
    #[error("background task panicked")]
    BackgroundTaskPanicked,
}

impl From<JoinError> for LocalTlsSetupError {
    fn from(_: JoinError) -> Self {
        Self::BackgroundTaskPanicked
    }
}

pub struct LazyTlsConnector {
    trust_roots: Option<Vec<PathBuf>>,
    config: OnceCell<ClientConfig>,
    server_name: Option<ServerName<'static>>,
}

impl LazyTlsConnector {
    pub fn new(
        trust_roots: Option<Vec<PathBuf>>,
        server_name: Option<ServerName<'static>>,
    ) -> Self {
        Self {
            trust_roots,
            config: Default::default(),
            server_name,
        }
    }

    pub async fn get(
        &self,
        alpn_protocol: Option<Vec<u8>>,
    ) -> Result<TlsConnector, LocalTlsSetupError> {
        let mut config = self
            .config
            .get_or_try_init(|| self.prepare_config())
            .await?
            .clone();
        config.alpn_protocols.extend(alpn_protocol);

        Ok(TlsConnector::from(Arc::new(config)))
    }

    pub fn server_name(&self) -> Option<&ServerName<'static>> {
        self.server_name.as_ref()
    }

    async fn prepare_config(&self) -> Result<ClientConfig, LocalTlsSetupError> {
        let builder = match self.trust_roots.as_ref() {
            Some(trust_roots) => {
                let paths = trust_roots.clone();
                let store = best_effort_root_store(paths).await?;

                if store.is_empty() {
                    return Err(LocalTlsSetupError::NoGoodRoot);
                }

                ClientConfig::builder().with_root_certificates(store)
            }
            None => ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(DangerousNoVerifierServer)),
        };

        Ok(builder.with_no_client_auth())
    }
}
