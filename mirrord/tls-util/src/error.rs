use std::io;

use rustls::{pki_types::InvalidDnsNameError, server::VerifierBuilderError};
use thiserror::Error;
use x509_parser::{
    error::{PEMError, X509Error},
    nom,
};

#[derive(Error, Debug)]
pub enum CertWithKeyError {
    #[error("failed to extract the certificate chain from the PEM file: {0}")]
    CertFromPemError(#[source] io::Error),
    #[error("failed to extract the private key from the PEM file: {0}")]
    KeyFromPemError(#[source] io::Error),
    #[error("the PEM file contains no certificates")]
    NoCertFound,
    #[error("the PEM file contains no private key")]
    NoKeyFound,
    #[error("the PEM file contains multiple private keys")]
    MultipleKeysFound,
}

#[derive(Error, Debug)]
pub enum SingleCertRootStoreError {
    #[error("failed to parse the PEM encoded data: {0}")]
    ParsePemError(#[from] nom::Err<PEMError>),
    #[error("failed to parse the certificate: {0}")]
    ParseCertError(#[from] nom::Err<X509Error>),
    #[error("the certificate contains an invalid or duplicate SAN extension: {0}")]
    InvalidSanExtension(#[source] X509Error),
    #[error("the certificate contains no SAN extension")]
    NoSanExtension,
    #[error("no valid SAN was found in the certificate")]
    NoSubjectAlternateName,
    #[error("failed to add the certificate: {0}")]
    BuildRootStoreError(#[from] rustls::Error),
}

#[derive(Error, Debug)]
pub enum BestEffortRootStoreError {
    #[error("failed to generate a dummy certificate: {0}")]
    GenerateDummyError(#[from] rcgen::Error),
    #[error("failed to add a dummy certificate: {0}")]
    AddDummyError(#[from] rustls::Error),
}

#[derive(Error, Debug)]
pub enum TlsUtilError {
    #[error("failed to build the certificate chain with the private key: {0}")]
    CertWithKeyError(#[from] CertWithKeyError),
    #[error("failed to build the single certificate root store: {0}")]
    SingleCertError(#[from] SingleCertRootStoreError),
    #[error("failed to build the best-effort certificate root store: {0}")]
    BestEffortRootStoreError(#[from] BestEffortRootStoreError),
    #[error("failed to build the certificate verifier: {0}")]
    VerifierBuilderError(#[from] VerifierBuilderError),
    #[error("failed to build the server config: {0}")]
    ServerConfigError(#[source] rustls::Error),
    #[error("failed to build the client config: {0}")]
    ClientConfigError(#[source] rustls::Error),
    #[error("request URI contains an invalid DNS name")]
    InvalidUriDnsName,
}

impl From<InvalidDnsNameError> for TlsUtilError {
    fn from(_: InvalidDnsNameError) -> Self {
        Self::InvalidUriDnsName
    }
}
