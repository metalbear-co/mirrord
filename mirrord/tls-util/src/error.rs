use std::{io, path::PathBuf};

use thiserror::Error;
use tokio::task::JoinError;

/// Errors that can occur when reading a certificate chain or a private key from a PEM file.
#[derive(Error, Debug)]
pub enum FromPemError {
    #[error("failed to open PEM file `{path}`: {error}")]
    OpenFileError {
        #[source]
        error: io::Error,
        path: PathBuf,
    },
    #[error("failed to parse PEM file `{path}`: {error}")]
    ParseFileError {
        #[source]
        error: io::Error,
        path: PathBuf,
    },
    #[error("blocking task panicked")]
    BlockingTaskPanicked,
    #[error("no certificate was found in PEM file `{0}`")]
    NoCertFound(PathBuf),
    #[error("multiple private keys were found in PEM file `{0}`")]
    MultipleKeysFound(PathBuf),
    #[error("no private key was found in PEM file `{0}`")]
    NoKeyFound(PathBuf),
}

impl From<JoinError> for FromPemError {
    fn from(_: JoinError) -> Self {
        Self::BlockingTaskPanicked
    }
}
