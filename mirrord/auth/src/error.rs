use thiserror::Error;
use x509_certificate::X509CertificateError;

/// Errors from [`CredentialStore`](crate::credential_store::CredentialStore) and
/// [`CredentialStoreSync`](crate::credential_store::CredentialStoreSync) operations
#[derive(Debug, Error)]
pub enum CredentialStoreError {
    #[error("failed to parent directory for credential store file: {0}")]
    ParentDir(std::io::Error),

    #[error("IO on credential store file failed: {0}")]
    FileAccess(std::io::Error),

    #[error("failed to lock/unlock credential store file: {0}")]
    Lockfile(std::io::Error),

    #[error("failed to serialize/deserialize credentials: {0}")]
    Yaml(#[from] serde_yaml::Error),

    #[error("x509 certificate error: {0}")]
    X509Certificate(#[from] X509CertificateError),

    #[error("certification request failed: {0}")]
    Kube(#[from] kube::Error),
}
