use rustls::pki_types::{CertificateDer, PrivateKeyDer};

use crate::{CertChain, TlsUtilError};

/// A random x509 certificate with a private key.
pub struct RandomCert {
    cert: CertificateDer<'static>,
    key: PrivateKeyDer<'static>,
}

impl RandomCert {
    /// Generates a new random self-signed certificate and a matching key.
    ///
    /// The certificate contains a SAN extension with the given names.
    pub fn generate(subject_alternate_names: Vec<String>) -> Result<Self, TlsUtilError> {
        let generated = rcgen::generate_simple_self_signed(subject_alternate_names)?;
        let key = generated
            .key_pair
            .serialize_der()
            .try_into()
            .map_err(|_| TlsUtilError::GeneratedInvalidKey)?;

        Ok(Self {
            cert: generated.cert.into(),
            key,
        })
    }

    pub fn cert(&self) -> &CertificateDer<'static> {
        &self.cert
    }

    pub fn key(&self) -> &PrivateKeyDer<'static> {
        &self.key
    }
}

impl From<RandomCert> for CertChain {
    fn from(value: RandomCert) -> Self {
        Self {
            cert_chain: vec![value.cert],
            key: value.key,
        }
    }
}

impl From<RandomCert> for CertificateDer<'static> {
    fn from(value: RandomCert) -> Self {
        value.cert
    }
}
