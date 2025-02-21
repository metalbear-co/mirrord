use std::{fs::File, io::BufReader, ops::Not, path::PathBuf};

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls_pemfile::Item;
use tracing::Level;

use crate::error::FromPemError;

/// Reads a certificate chain from the given PEM file.
///
/// 1. PEM items of other types are ignored.
/// 2. At least one certificate is required.
/// 3. Certificates are not verified in any way.
///
/// All logic is done in a blocking task. See this crate's doc for rationale.
#[tracing::instrument(level = Level::DEBUG, ret, err(level = Level::DEBUG))]
pub async fn read_cert_chain(path: PathBuf) -> Result<Vec<CertificateDer<'static>>, FromPemError> {
    tokio::task::spawn_blocking(move || {
        let mut file = match File::open(&path) {
            Ok(file) => BufReader::new(file),
            Err(error) => return Err(FromPemError::OpenFileError { error, path }),
        };

        let cert_chain = rustls_pemfile::certs(&mut file).collect::<Result<Vec<_>, _>>();

        match cert_chain {
            Ok(cert_chain) if cert_chain.is_empty().not() => Ok(cert_chain),
            Ok(..) => Err(FromPemError::NoCertFound(path)),
            Err(error) => Err(FromPemError::ParseFileError { error, path }),
        }
    })
    .await?
}

/// Reads a private key from the given PEM file.
///
/// 1. PEM items of other types are ignored.
/// 2. Exactly one private key is required.
///
/// All logic is done in a blocking task. See this crate's doc for rationale.
#[tracing::instrument(level = Level::DEBUG, ret, err(level = Level::DEBUG))]
pub async fn read_key_der(path: PathBuf) -> Result<PrivateKeyDer<'static>, FromPemError> {
    tokio::task::spawn_blocking(move || {
        let mut file = match File::open(&path) {
            Ok(file) => BufReader::new(file),
            Err(error) => return Err(FromPemError::OpenFileError { error, path }),
        };

        let mut found_key = None;

        for entry in rustls_pemfile::read_all(&mut file) {
            let key = match entry {
                Ok(Item::Pkcs1Key(key)) => PrivateKeyDer::Pkcs1(key),
                Ok(Item::Pkcs8Key(key)) => PrivateKeyDer::Pkcs8(key),
                Ok(Item::Sec1Key(key)) => PrivateKeyDer::Sec1(key),
                Ok(..) => continue,
                Err(error) => return Err(FromPemError::ParseFileError { error, path }),
            };

            if found_key.replace(key).is_some() {
                return Err(FromPemError::MultipleKeysFound(path));
            }
        }

        found_key.ok_or(FromPemError::NoKeyFound(path))
    })
    .await?
}
