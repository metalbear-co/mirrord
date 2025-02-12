use std::sync::Arc;

use rustls::ClientConfig;
use tokio_rustls::TlsConnector;

use crate::{
    best_effort_root_store::BestEffortRootStore, no_verifier::NoVerifier,
    single_cert_root_store::SingleCertRootStore, CertWithKey, TlsUtilError,
};

pub enum ConnectorServerAuth {
    DangerousAcceptAny,
    SingleCertRootStore(SingleCertRootStore),
    BestEffortRootStore(BestEffortRootStore),
}

pub enum ConnectorClientAuth {
    Anonymous,
    CertWithKey(CertWithKey),
}

pub struct TlsConnectorConfig {
    pub server_auth: ConnectorServerAuth,
    pub client_auth: ConnectorClientAuth,
}

pub trait TlsConnectorExt: Sized {
    fn build_from_config(config: TlsConnectorConfig) -> Result<Self, TlsUtilError>;
}

impl TlsConnectorExt for TlsConnector {
    fn build_from_config(config: TlsConnectorConfig) -> Result<Self, TlsUtilError> {
        let builder = match config.server_auth {
            ConnectorServerAuth::DangerousAcceptAny => ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(NoVerifier)),
            ConnectorServerAuth::SingleCertRootStore(store) => {
                ClientConfig::builder().with_root_certificates(store)
            }
            ConnectorServerAuth::BestEffortRootStore(store) => {
                ClientConfig::builder().with_root_certificates(store)
            }
        };

        let config = match config.client_auth {
            ConnectorClientAuth::Anonymous => builder.with_no_client_auth(),
            ConnectorClientAuth::CertWithKey(cert_with_key) => builder
                .with_client_auth_cert(cert_with_key.cert_chain, cert_with_key.key)
                .map_err(TlsUtilError::ClientConfigError)?,
        };

        Ok(TlsConnector::from(Arc::new(config)))
    }
}
