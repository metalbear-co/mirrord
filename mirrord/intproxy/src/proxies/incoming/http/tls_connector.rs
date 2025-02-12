use std::{
    fmt,
    fs::File,
    io::{self, BufReader},
    path::{Path, PathBuf},
    sync::Arc,
};

use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime},
    ClientConfig, DigitallySignedStruct, SignatureScheme,
};
use rustls_pemfile::Item;
use thiserror::Error;
use tokio::{sync::OnceCell, task::JoinError};
use tokio_rustls::TlsConnector;
use tracing::Level;

#[derive(Error, Debug)]
pub enum LazyConnectorError {
    #[error("background task responsible for parsing PEM files panicked")]
    BackgroundTaskPanicked,

    #[error("failed to open PEM file `{path}`: {error}")]
    PemOpenError {
        path: PathBuf,
        #[source]
        error: io::Error,
    },

    #[error("failed to parse PEM file `{path}`: {error}")]
    PemParseError {
        path: PathBuf,
        #[source]
        error: io::Error,
    },

    #[error("no certificated were found in PEM file `{0}`")]
    CertChainEmpty(PathBuf),

    #[error("no private key was found in PEM file `{0}`")]
    KeyNotFound(PathBuf),

    #[error("multiple private keys were found in PEM file `{0}`")]
    MultipleKeysFound(PathBuf),

    #[error("failed to build TLS client config: {0}")]
    ConfigBuildError(#[source] rustls::Error),
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
        self.connector
            .get_or_try_init(|| self.build_connector())
            .await
    }

    #[tracing::instrument(level = Level::TRACE, err)]
    async fn build_connector(&self) -> Result<TlsConnector, LazyConnectorError> {
        let builder = ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerifier));

        let config = match self.cert_and_key.as_ref() {
            Some((cert_pem, key_pem)) => {
                let cert_pem = cert_pem.clone();
                let key_pem = key_pem.clone();
                let (cert_chain, key) = tokio::task::spawn_blocking(move || {
                    let cert_chain = Self::get_cert_chain(&cert_pem)?;
                    let key = Self::get_key(&key_pem)?;
                    Ok::<_, LazyConnectorError>((cert_chain, key))
                })
                .await??;

                builder
                    .with_client_auth_cert(cert_chain, key)
                    .map_err(LazyConnectorError::ConfigBuildError)?
            }
            None => builder.with_no_client_auth(),
        };

        Ok(TlsConnector::from(Arc::new(config)))
    }

    fn get_cert_chain(path: &Path) -> Result<Vec<CertificateDer<'static>>, LazyConnectorError> {
        let pem = File::open(path).map_err(|error| LazyConnectorError::PemOpenError {
            path: path.to_path_buf(),
            error,
        })?;

        let cert_chain = rustls_pemfile::certs(&mut BufReader::new(pem))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| LazyConnectorError::PemParseError {
                path: path.to_path_buf(),
                error,
            })?;

        if cert_chain.is_empty() {
            return Err(LazyConnectorError::CertChainEmpty(path.to_path_buf()));
        }

        Ok(cert_chain)
    }

    fn get_key(path: &Path) -> Result<PrivateKeyDer<'static>, LazyConnectorError> {
        let pem = File::open(path).map_err(|error| LazyConnectorError::PemOpenError {
            path: path.to_path_buf(),
            error,
        })?;

        let mut found_key = None;

        for entry in rustls_pemfile::read_all(&mut BufReader::new(pem)) {
            let entry = entry.map_err(|error| LazyConnectorError::PemParseError {
                path: path.to_path_buf(),
                error,
            })?;

            let key = match entry {
                Item::Pkcs1Key(key) => PrivateKeyDer::Pkcs1(key),
                Item::Pkcs8Key(key) => PrivateKeyDer::Pkcs8(key),
                Item::Sec1Key(key) => PrivateKeyDer::Sec1(key),
                _ => continue,
            };

            if found_key.replace(key).is_some() {
                return Err(LazyConnectorError::MultipleKeysFound(path.to_path_buf()));
            }
        }

        found_key.ok_or(LazyConnectorError::KeyNotFound(path.to_path_buf()))
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

#[derive(Debug)]
struct NoVerifier;

impl ServerCertVerifier for NoVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer,
        _intermediates: &[CertificateDer],
        _server_name: &ServerName,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::RSA_PKCS1_SHA1,
            SignatureScheme::ECDSA_SHA1_Legacy,
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
            SignatureScheme::ECDSA_NISTP521_SHA512,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
            SignatureScheme::ED25519,
            SignatureScheme::ED448,
        ]
    }
}
