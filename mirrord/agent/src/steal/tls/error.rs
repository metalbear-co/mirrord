use std::{io, path::PathBuf};

use mirrord_tls_util::{
    rustls::{self, server::VerifierBuilderError},
    TlsUtilError,
};
use thiserror::Error;
use tokio::task::JoinError;

/// Errors that can occur when building [`StealTlsHandler`](super::StealTlsHandler) components..
#[derive(Error, Debug)]
pub enum SetupError {
    #[error("failed to read certificate chain: {0}")]
    ReadCertChainError(#[source] TlsUtilError),
    #[error("failed to generate a dummy certificate: {0}")]
    GenerateDummyCertError(#[source] TlsUtilError),
    #[error("generated an invalid dummy certificate: {0}")]
    DummyCertInvalid(#[source] rustls::Error),
    #[error("no good root certificate was found")]
    NoGoodRoot,
    #[error("failed to build the peer verifier: {0}")]
    BuildVerifierError(#[from] VerifierBuilderError),
    #[error("certificate chain is invalid: {0}")]
    CertChainInvalid(#[source] rustls::Error),
    #[error("failed to resolve path `{path}` in the target container filesystem: {error}")]
    PathResolutionError { path: PathBuf, error: io::Error },
}

/// Errors that can occur when building a [`StealTlsHandler`](super::StealTlsHandler).
#[derive(Error, Debug)]
pub enum StealTlsError {
    /// Building is done in a blocking tokio task due to its heavy computational nature.
    #[error("background task panicked")]
    BackgroundTaskPanicked,
    /// Failure when building a [`TlsAcceptor`](mirrord_tls_util::tokio_rustls::TlsAcceptor).
    #[error("failed to prepare mirrord-agent's TLS server: {0}")]
    ServerSetupError(#[source] SetupError),
    /// Failure when building a [`TlsConnector`](mirrord_tls_util::tokio_rustls::TlsConnector).
    #[error("failed to prepare mirrord-agent's TLS client: {0}")]
    ClientSetupError(#[source] SetupError),
}

impl From<JoinError> for StealTlsError {
    fn from(_: JoinError) -> Self {
        Self::BackgroundTaskPanicked
    }
}
