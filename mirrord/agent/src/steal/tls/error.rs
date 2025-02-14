use std::{io, path::PathBuf, sync::PoisonError};

use rustls::server::VerifierBuilderError;
use thiserror::Error;
use tokio::task::JoinError;

/// Errors that can occur when building a [`StealTlsHandler`](super::handler::StealTlsHandler)
/// with [`StealTlsHandlerStore`](super::StealTlsHandlerStore).
#[derive(Error, Debug)]
pub enum StealTlsSetupError {
    /// Building is done in a blocking [`tokio::task`],
    /// because it's heavy computationally.
    #[error("background task panicked")]
    BackgroundTaskPanicked,
    #[error("TLS handlers store mutex is poisoned")]
    MutexPoisoned,
    #[error("failed to build mirrord-agent's TLS server: {0}")]
    ServerSetupError(#[source] StealTlsSetupErrorInner),
    #[error("failed to build mirrord-agent's TLS client: {0}")]
    ClientSetupError(#[source] StealTlsSetupErrorInner),
}

impl From<JoinError> for StealTlsSetupError {
    fn from(_: JoinError) -> Self {
        Self::BackgroundTaskPanicked
    }
}

impl<T> From<PoisonError<T>> for StealTlsSetupError {
    fn from(_: PoisonError<T>) -> Self {
        Self::MutexPoisoned
    }
}

/// Errors that can occure when building mirrord-agent's TLS server or client.
#[derive(Error, Debug)]
pub enum StealTlsSetupErrorInner {
    #[error("failed to resolve path `{path}` in the target container filesystem: {error}")]
    PathResolutionError {
        #[source]
        error: io::Error,
        path: PathBuf,
    },
    #[error("no good trust root certificate was found")]
    NoGoodRoot,
    #[error("generated an invalid dummy certificate: {0}")]
    GeneratedInvalidDummy(#[source] rustls::Error),
    #[error("failed to generate a dummy certificate: {0}")]
    GenerateDummyError(#[from] rcgen::Error),
    #[error("failed to build a certificate verifier: {0}")]
    VerifierBuilderError(#[from] VerifierBuilderError),
    #[error("failed to parse PEM file `{path}`: {error}")]
    ParsePemError {
        #[source]
        error: io::Error,
        path: PathBuf,
    },
    #[error("no certificate was found in PEM file `{0}`")]
    NoCertFound(PathBuf),
    #[error("multiple private keys were found in PEM file `{0}`")]
    MultipleKeysFound(PathBuf),
    #[error("no private key was found in PEM file `{0}`")]
    NoKeyFound(PathBuf),
    #[error("certificate chain is invalid: {0}")]
    CertChainInvalid(#[source] rustls::Error),
}
