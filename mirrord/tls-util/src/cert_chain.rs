use std::{fmt, fs::File, io::BufReader};

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls_pemfile::Item;

use crate::{error::TlsUtilError, NicePath};

pub struct CertChain {
    pub(crate) cert_chain: Vec<CertificateDer<'static>>,
    pub(crate) key: PrivateKeyDer<'static>,
}

impl fmt::Debug for CertChain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CertChain")
            .field("certifiactes", &self.cert_chain.len())
            .finish()
    }
}

impl CertChain {
    pub fn read<P: NicePath + ?Sized>(cert_pem: &P, key_pem: &P) -> Result<Self, TlsUtilError> {
        let cert_chain = Self::read_cert_chain(cert_pem)?;
        let key = Self::read_key(key_pem)?;

        Ok(Self { cert_chain, key })
    }

    pub fn cert_chain(&self) -> &[CertificateDer<'static>] {
        &self.cert_chain
    }

    pub fn key(&self) -> &PrivateKeyDer<'static> {
        &self.key
    }

    pub fn into_chain_and_key(self) -> (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>) {
        (self.cert_chain, self.key)
    }

    fn read_cert_chain<P: NicePath + ?Sized>(
        path: &P,
    ) -> Result<Vec<CertificateDer<'static>>, TlsUtilError> {
        let pem =
            File::open(path.real_path()).map_err(|error| TlsUtilError::ParsePemFileError {
                error,
                path: path.display_path().to_path_buf(),
            })?;

        let cert_chain = rustls_pemfile::certs(&mut BufReader::new(pem))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| TlsUtilError::ParsePemFileError {
                error,
                path: path.display_path().to_path_buf(),
            })?;

        if cert_chain.is_empty() {
            return Err(TlsUtilError::NoCertFound(path.display_path().to_path_buf()));
        }

        Ok(cert_chain)
    }

    fn read_key<P: NicePath + ?Sized>(path: &P) -> Result<PrivateKeyDer<'static>, TlsUtilError> {
        let pem =
            File::open(path.real_path()).map_err(|error| TlsUtilError::ParsePemFileError {
                error,
                path: path.display_path().to_path_buf(),
            })?;

        let mut found_key = None;

        for entry in rustls_pemfile::read_all(&mut BufReader::new(pem)) {
            let entry = entry.map_err(|error| TlsUtilError::ParsePemFileError {
                error,
                path: path.display_path().to_path_buf(),
            })?;
            let key = match entry {
                Item::Pkcs1Key(key) => PrivateKeyDer::Pkcs1(key),
                Item::Pkcs8Key(key) => PrivateKeyDer::Pkcs8(key),
                Item::Sec1Key(key) => PrivateKeyDer::Sec1(key),
                _ => continue,
            };

            if found_key.replace(key).is_some() {
                return Err(TlsUtilError::MultipleKeysFound(
                    path.display_path().to_path_buf(),
                ));
            }
        }

        found_key.ok_or(TlsUtilError::NoKeyFound(path.display_path().to_path_buf()))
    }
}

#[cfg(test)]
mod test {
    use std::fs;

    use rustls::RootCertStore;

    use super::CertChain;
    use crate::{AsPem, RandomCert};

    #[test]
    fn generate_and_parse() {
        let tmpdir = tempfile::tempdir().unwrap();
        let cert_file = tmpdir.path().join("cert.pem");
        let generated = RandomCert::generate(vec!["operator".into()]).unwrap();
        let cert_pem = generated.cert().as_pem();
        fs::write(&cert_file, cert_pem.as_bytes()).unwrap();

        let key_file = tmpdir.path().join("key.pem");
        let key_pem = generated.key().as_pem();
        fs::write(&key_file, key_pem.as_bytes()).unwrap();

        let parsed = CertChain::read(&cert_file, &key_file).unwrap();
        assert_eq!(parsed.cert_chain(), vec![generated.cert().clone()]);
        assert_eq!(parsed.key(), generated.key());

        // Verify that the certificate can be used as a trust anchor.
        let mut root_store = RootCertStore::empty();
        for cert in parsed.into_chain_and_key().0 {
            root_store.add(cert).unwrap();
        }
    }
}
