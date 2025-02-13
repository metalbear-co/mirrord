use std::{fmt, io, path::PathBuf, sync::Arc};

use mirrord_tls_util::{
    rustls::{self, pki_types::ServerName, ClientConfig},
    tokio_rustls::{client::TlsStream, TlsConnector},
    CertChain, DangerousNoVerifier, TlsUtilError,
};
use thiserror::Error;
use tokio::{net::TcpStream, task::JoinError};

#[derive(Error, Debug)]
pub enum LocalTlsConnectorError {
    #[error("background task panicked")]
    BackgroundTaskPanicked,
    #[error("failed to parse internal proxy certicate chain: {0}")]
    ParseIntproxyCertError(#[from] TlsUtilError),
    #[error("internal proxy certificate chain is invalid: {0}")]
    InvalidIntproxyCertError(#[from] rustls::Error),
}

impl From<JoinError> for LocalTlsConnectorError {
    fn from(_: JoinError) -> Self {
        Self::BackgroundTaskPanicked
    }
}

pub struct LocalTlsConnector {
    config: Arc<ClientConfig>,
}

impl LocalTlsConnector {
    pub async fn connect(
        &self,
        server_name: ServerName<'static>,
        stream: TcpStream,
    ) -> io::Result<TlsStream<TcpStream>> {
        let connector = TlsConnector::from(self.config.clone());
        connector.connect(server_name, stream).await
    }

    pub async fn anonymous(alpn_protocol: Option<Vec<u8>>) -> Result<Self, LocalTlsConnectorError> {
        tokio::task::spawn_blocking(move || Self::build(None, alpn_protocol)).await?
    }

    pub async fn authenticated(
        cert_pem: PathBuf,
        key_pem: PathBuf,
        alpn_protocol: Option<Vec<u8>>,
    ) -> Result<Self, LocalTlsConnectorError> {
        tokio::task::spawn_blocking(move || Self::build(Some((cert_pem, key_pem)), alpn_protocol))
            .await?
    }

    fn build(
        cert_and_key: Option<(PathBuf, PathBuf)>,
        alpn_protocol: Option<Vec<u8>>,
    ) -> Result<Self, LocalTlsConnectorError> {
        let builder = ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(DangerousNoVerifier));

        let mut config = match cert_and_key {
            Some((cert, key)) => {
                let chain = CertChain::read(&cert, &key)
                    .map_err(LocalTlsConnectorError::ParseIntproxyCertError)?;
                let (cert_chain, key_der) = chain.into_chain_and_key();
                builder
                    .with_client_auth_cert(cert_chain, key_der)
                    .map_err(LocalTlsConnectorError::InvalidIntproxyCertError)?
            }
            None => builder.with_no_client_auth(),
        };

        config.alpn_protocols = alpn_protocol.map(|proto| vec![proto]).unwrap_or_default();

        Ok(Self {
            config: config.into(),
        })
    }

    pub fn alpn_protocol(&self) -> Option<&[u8]> {
        Some(self.config.alpn_protocols.first()?.as_slice())
    }
}

impl fmt::Debug for LocalTlsConnector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let alpn_protocol = self.alpn_protocol().map(String::from_utf8_lossy);
        f.debug_struct("LocalTlsConnector")
            .field("alpn_protocol", &alpn_protocol)
            .finish()
    }
}
