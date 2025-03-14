use std::{io, path::PathBuf};

use rustls::server::VerifierBuilderError;
use thiserror::Error;
use tokio::task::JoinError;
use x509_parser::{error::X509Error, nom};

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

/// Errors that can occur when extracting Subject Alternate Names from a certificate.
#[derive(Error, Debug)]
pub enum GetSanError {
    #[error("SAN extension is invalid or present more than once")]
    InvalidSanExtension(#[source] X509Error),
    #[error("SAN extension was not found")]
    NoSanExtension,
    #[error("failed to parse the x509 certificate: {0}")]
    ParseDerError(#[from] nom::Err<X509Error>),
}

/// Errors that can occur when preparing or using
/// [`SecureChannelSetup`](crate::secure_channel::SecureChannelSetup).
#[derive(Error, Debug)]
pub enum SecureChannelError {
    #[error("failed to generate a random certificate: {0}")]
    GenerateError(#[from] rcgen::Error),
    #[error("failed to prepare a temporary PEM file: {0}")]
    TmpFileCreateError(#[from] io::Error),
    #[error(transparent)]
    FromPemError(#[from] FromPemError),
    #[error("no root certificate was found in the PEM file")]
    NoRootCert,
    #[error("root certificate found in the PEM file was invalid: {0}")]
    InvalidRootCert(#[source] rustls::Error),
    #[error("failed to build client verifier: {0}")]
    VerifierBuildError(#[from] VerifierBuilderError),
    #[error("certificate chain found in the PEM file was invalid: {0}")]
    InvalidCertChain(#[source] rustls::Error),
}
