use std::sync::Arc;

use rustls::{server::WebPkiClientVerifier, ServerConfig};
use tokio_rustls::TlsAcceptor;

use crate::{best_effort_root_store::BestEffortRootStore, CertWithKey, TlsUtilError};

pub enum AcceptorClientAuth {
    Disabled,
    WithRootStore {
        store: BestEffortRootStore,
        allow_unauthenticated: bool,
    },
}

pub struct TlsAcceptorConfig {
    pub server_auth: CertWithKey,
    pub client_auth: AcceptorClientAuth,
}

pub trait TlsAcceptorExt: Sized {
    fn build_from_config(config: TlsAcceptorConfig) -> Result<Self, TlsUtilError>;
}

impl TlsAcceptorExt for TlsAcceptor {
    fn build_from_config(config: TlsAcceptorConfig) -> Result<Self, TlsUtilError> {
        let builder = match config.client_auth {
            AcceptorClientAuth::Disabled => ServerConfig::builder().with_no_client_auth(),
            AcceptorClientAuth::WithRootStore {
                mut store,
                allow_unauthenticated,
            } => {
                if store.certs() == 0 && allow_unauthenticated {
                    store.add_dummy()?;
                }

                let verifier = WebPkiClientVerifier::builder(store.into()).build()?;

                ServerConfig::builder().with_client_cert_verifier(verifier)
            }
        };

        let config = builder
            .with_single_cert(config.server_auth.cert_chain, config.server_auth.key)
            .map_err(TlsUtilError::ServerConfigError)?;

        Ok(TlsAcceptor::from(Arc::new(config)))
    }
}
