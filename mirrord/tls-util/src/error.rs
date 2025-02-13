use std::{io, path::PathBuf};

use thiserror::Error;
use x509_parser::{
    error::{PEMError, X509Error},
    nom,
};

#[derive(Error, Debug)]
pub enum TlsUtilError {
    #[error("failed to parse the PEM file at `{path}`: {error}")]
    ParsePemFileError {
        #[source]
        error: io::Error,
        path: PathBuf,
    },
    #[error("the PEM file at `{0}` contains no certificates")]
    NoCertFound(PathBuf),
    #[error("the PEM file at `{0}` contains no private key")]
    NoKeyFound(PathBuf),
    #[error("the PEM file at `{0}` contains multiple private keys")]
    MultipleKeysFound(PathBuf),
    #[error("failed to parse the PEM encoded data: {0}")]
    ParsePemDataError(#[from] nom::Err<PEMError>),
    #[error("failed to parse the x509 certificate: {0}")]
    ParseX509CertError(#[from] nom::Err<X509Error>),
    #[error("the certificate contains an invalid or duplicate SAN extension: {0}")]
    InvalidSanExtension(#[source] X509Error),
    #[error("the certificate contains no SAN extension")]
    NoSanExtension,
    #[error("no valid SAN was found in the certificate")]
    NoSubjectAlternateName,
    #[error("failed to generate a random certificate: {0}")]
    GenerateError(#[from] rcgen::Error),
    #[error("generated an invalid private key")]
    GeneratedInvalidKey,
    #[error("generated an invalid certificate")]
    GeneratedInvalidCertificate(#[from] rustls::Error),
}
