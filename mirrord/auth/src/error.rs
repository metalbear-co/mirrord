use thiserror::Error;
use x509_certificate::X509CertificateError;

#[derive(Debug, Error)]
pub enum AuthenticationError {
    #[error(transparent)]
    Pem(std::io::Error),
    #[error(transparent)]
    X509Certificate(#[from] X509CertificateError),
    #[cfg(feature = "client")]
    #[error(transparent)]
    CertificateStore(#[from] CertificateStoreError),
    #[cfg(feature = "client")]
    #[error(transparent)]
    Kube(#[from] kube::Error),
}

#[cfg(feature = "client")]
#[derive(Debug, Error)]
pub enum CertificateStoreError {
    #[error("Unable to save/load CertificateStore: {0}")]
    Io(#[from] std::io::Error),
    #[error("Unable to create CertificateStore lockfile: {0}")]
    Lockfile(std::io::Error),
    #[error("Unable serialize/deserialize CertificateStore: {0}")]
    Yaml(#[from] serde_yaml::Error),
}

pub type Result<T, E = AuthenticationError> = std::result::Result<T, E>;
