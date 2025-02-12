use std::{fmt, path::PathBuf};

use mirrord_tls_util::{
    tokio_rustls::TlsConnector, CertWithKey, ConnectorClientAuth, ConnectorServerAuth,
    TlsConnectorConfig, TlsConnectorExt, TlsUtilError,
};
use tokio::sync::OnceCell;
use tracing::Level;

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

    pub async fn get(&self) -> Result<&TlsConnector, TlsUtilError> {
        let connector = self
            .connector
            .get_or_try_init(|| self.build_connector())
            .await?;

        Ok(connector)
    }

    #[tracing::instrument(level = Level::TRACE, err)]
    async fn build_connector(&self) -> Result<TlsConnector, TlsUtilError> {
        let client_auth = match self.cert_and_key.as_ref() {
            Some((cert_pem, key_pem)) => {
                let cert_pem = cert_pem.clone();
                let key_pem = key_pem.clone();
                let cert_with_key =
                    tokio::task::spawn_blocking(move || CertWithKey::read(&cert_pem, &key_pem))
                        .await
                        .expect("this task does not panic")?;

                ConnectorClientAuth::CertWithKey(cert_with_key)
            }
            None => ConnectorClientAuth::Anonymous,
        };

        let config = TlsConnectorConfig {
            server_auth: ConnectorServerAuth::DangerousAcceptAny,
            client_auth,
        };

        let connector = TlsConnector::build_from_config(config)?;

        Ok(connector)
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
