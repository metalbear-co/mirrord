use thiserror::Error;
use x509_certificate::X509CertificateError;

#[derive(Debug, Error)]
pub enum AuthenticationError {
    #[error(transparent)]
    CertificateStore(#[from] CertificateStoreError),
    #[error(transparent)]
    Kube(#[from] kube::Error),
    #[error(transparent)]
    X509Certificate(#[from] X509CertificateError),
}

#[derive(Debug, Error)]
pub enum CertificateStoreError {
    #[error("Unable to save/load CertificateStore: {0}")]
    Io(#[from] std::io::Error),
    #[error("Unable serialize/deserialize CertificateStore: {0}")]
    Yaml(#[from] serde_yaml::Error),
}

pub type Result<T, E = AuthenticationError> = std::result::Result<T, E>;
