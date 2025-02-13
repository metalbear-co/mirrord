use rustls::pki_types::{CertificateDer, PrivateKeyDer};

use crate::{CertChain, TlsUtilError};

pub struct RandomCert {
    cert: CertificateDer<'static>,
    key: PrivateKeyDer<'static>,
}

impl RandomCert {
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
