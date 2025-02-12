use std::{fs::File, io::BufReader, path::Path};

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls_pemfile::Item;

use crate::error::CertWithKeyError;

pub struct CertWithKey {
    pub(crate) cert_chain: Vec<CertificateDer<'static>>,
    pub(crate) key: PrivateKeyDer<'static>,
}

impl CertWithKey {
    pub fn read(cert_pem: &Path, key_pem: &Path) -> Result<Self, CertWithKeyError> {
        let cert_chain = Self::get_cert_chain(cert_pem)?;
        let key = Self::get_key(key_pem)?;

        Ok(Self { cert_chain, key })
    }

    pub fn new_random_self_signed<I: Into<Vec<String>>>(
        subject_alternate_names: I,
    ) -> Result<Self, CertWithKeyError> {
        let generated = rcgen::generate_simple_self_signed(subject_alternate_names)?;
        let cert_chain = vec![generated.cert.into()];
        let key = generated
            .key_pair
            .serialize_der()
            .try_into()
            .map_err(|_| CertWithKeyError::GeneratedInvalidKey)?;

        Ok(Self { cert_chain, key })
    }

    pub fn cert_chain(&self) -> &[CertificateDer<'static>] {
        &self.cert_chain
    }

    pub fn key(&self) -> &PrivateKeyDer<'static> {
        &self.key
    }

    fn get_cert_chain(path: &Path) -> Result<Vec<CertificateDer<'static>>, CertWithKeyError> {
        let pem = File::open(path).map_err(CertWithKeyError::CertFromPemError)?;

        let cert_chain = rustls_pemfile::certs(&mut BufReader::new(pem))
            .collect::<Result<Vec<_>, _>>()
            .map_err(CertWithKeyError::CertFromPemError)?;

        if cert_chain.is_empty() {
            return Err(CertWithKeyError::NoCertFound);
        }

        Ok(cert_chain)
    }

    fn get_key(path: &Path) -> Result<PrivateKeyDer<'static>, CertWithKeyError> {
        let pem = File::open(path).map_err(CertWithKeyError::KeyFromPemError)?;

        let mut found_key = None;

        for entry in rustls_pemfile::read_all(&mut BufReader::new(pem)) {
            let entry = entry.map_err(CertWithKeyError::KeyFromPemError)?;
            let key = match entry {
                Item::Pkcs1Key(key) => PrivateKeyDer::Pkcs1(key),
                Item::Pkcs8Key(key) => PrivateKeyDer::Pkcs8(key),
                Item::Sec1Key(key) => PrivateKeyDer::Sec1(key),
                _ => continue,
            };

            if found_key.replace(key).is_some() {
                return Err(CertWithKeyError::MultipleKeysFound);
            }
        }

        found_key.ok_or(CertWithKeyError::NoKeyFound)
    }
}
