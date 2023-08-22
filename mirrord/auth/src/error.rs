use thiserror::Error;
use x509_certificate::X509CertificateError;

/// Wrapper error for errors in mirrord-auth library
#[derive(Debug, Error)]
pub enum AuthenticationError {
    /// Error from parsing `pem` wrapped certificate/key-pair
    #[error(transparent)]
    Pem(std::io::Error),

    /// Error from from generating sha256 fingerprint for certificate/key-pair
    #[error(transparent)]
    Fingerprint(std::io::Error),

    /// Error from `x509_certificate` library
    #[error(transparent)]
    X509Certificate(#[from] X509CertificateError),

    #[cfg(feature = "client")]
    #[error(transparent)]
    CertificateStore(#[from] CertificateStoreError),

    #[cfg(feature = "client")]
    #[error(transparent)]
    Kube(#[from] kube::Error),
}

/// Error from CredentialStore operations
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

/// `Result` with `AuthenticationError` as default error
pub type Result<T, E = AuthenticationError> = std::result::Result<T, E>;
