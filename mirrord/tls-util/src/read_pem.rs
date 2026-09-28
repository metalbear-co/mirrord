use std::{
    fs::File,
    io::{BufRead, BufReader},
    ops::Not,
    path::PathBuf,
};

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls_pemfile::Item;
use tracing::Level;

use crate::error::{FromPemError, ParsePemError};

/// Parses a certificate chain from PEM data.
///
/// 1. PEM items of other types are ignored.
/// 2. At least one certificate is required.
/// 3. Certificates are not verified in any way.
pub fn parse_cert_chain(
    mut pem: impl BufRead,
) -> Result<Vec<CertificateDer<'static>>, ParsePemError> {
    let cert_chain = rustls_pemfile::certs(&mut pem).collect::<Result<Vec<_>, _>>();

    match cert_chain {
        Ok(cert_chain) if cert_chain.is_empty().not() => Ok(cert_chain),
        Ok(..) => Err(ParsePemError::NoCert),
        Err(error) => Err(ParsePemError::Parse(error)),
    }
}

/// Parses a private key from PEM data.
///
/// 1. PEM items of other types are ignored.
/// 2. Exactly one private key is required.
pub fn parse_key_der(mut pem: impl BufRead) -> Result<PrivateKeyDer<'static>, ParsePemError> {
    let mut found_key = None;

    for entry in rustls_pemfile::read_all(&mut pem) {
        let key = match entry {
            Ok(Item::Pkcs1Key(key)) => PrivateKeyDer::Pkcs1(key),
            Ok(Item::Pkcs8Key(key)) => PrivateKeyDer::Pkcs8(key),
            Ok(Item::Sec1Key(key)) => PrivateKeyDer::Sec1(key),
            Ok(..) => continue,
            Err(error) => return Err(ParsePemError::Parse(error)),
        };

        if found_key.replace(key).is_some() {
            return Err(ParsePemError::MultipleKeys);
        }
    }

    found_key.ok_or(ParsePemError::NoKey)
}

/// Reads a certificate chain from the given PEM file, see [`parse_cert_chain`].
///
/// All logic is done in a blocking task. See this crate's doc for rationale.
#[tracing::instrument(level = Level::DEBUG, ret, err(level = Level::DEBUG))]
pub async fn read_cert_chain(path: PathBuf) -> Result<Vec<CertificateDer<'static>>, FromPemError> {
    tokio::task::spawn_blocking(move || {
        let file = match File::open(&path) {
            Ok(file) => BufReader::new(file),
            Err(error) => return Err(FromPemError::OpenFileError { error, path }),
        };

        parse_cert_chain(file).map_err(|error| error.in_file(path))
    })
    .await?
}

/// Reads a private key from the given PEM file, see [`parse_key_der`].
///
/// All logic is done in a blocking task. See this crate's doc for rationale.
#[tracing::instrument(level = Level::DEBUG, ret, err(level = Level::DEBUG))]
pub async fn read_key_der(path: PathBuf) -> Result<PrivateKeyDer<'static>, FromPemError> {
    tokio::task::spawn_blocking(move || {
        let file = match File::open(&path) {
            Ok(file) => BufReader::new(file),
            Err(error) => return Err(FromPemError::OpenFileError { error, path }),
        };

        parse_key_der(file).map_err(|error| error.in_file(path))
    })
    .await?
}
