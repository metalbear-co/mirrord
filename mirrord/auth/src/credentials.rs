use std::{fmt::Debug, ops::Deref};

use kube::{api::PostParams, Api, Client, Resource};
use serde::{Deserialize, Serialize};
pub use x509_certificate;
use x509_certificate::{
    rfc2986, InMemorySigningKeyPair, KeyAlgorithm, X509CertificateBuilder, X509CertificateError,
};

use crate::{
    certificate::Certificate,
    error::{AuthenticationError, Result},
    key_pair::KeyPair,
};

#[derive(Debug, Serialize, Deserialize)]
pub struct Credentials {
    certificate: Option<Certificate>,
    key_pair: KeyPair,
}

impl Credentials {
    pub fn init() -> Result<Self> {
        let key_algorithm = KeyAlgorithm::Ed25519;
        let (_, document) = InMemorySigningKeyPair::generate_random(key_algorithm)?;

        let pem_key = pem::Pem::new("PRIVATE KEY", document.as_ref());

        Ok(Credentials {
            certificate: None,
            key_pair: pem::encode(&pem_key).into(),
        })
    }

    pub fn is_ready(&self) -> bool {
        self.certificate.is_some()
    }

    pub fn certificate_request(&self, cn: &str) -> Result<rfc2986::CertificationRequest> {
        let mut builder = X509CertificateBuilder::new(KeyAlgorithm::Ed25519);

        let _ = builder.subject().append_common_name_utf8_string(cn);

        builder
            .create_certificate_signing_request(self.key_pair.deref())
            .map_err(AuthenticationError::from)
    }

    pub async fn get_client_certificate<R>(&mut self, client: Client, cn: &str) -> Result<()>
    where
        R: Resource + Clone + Debug,
        R: for<'de> Deserialize<'de>,
        R::DynamicType: Default,
    {
        let certificate_request = self
            .certificate_request(cn)?
            .encode_pem()
            .map_err(X509CertificateError::from)?;

        let api: Api<R> = Api::all(client);

        let certificate: Certificate = api
            .create_subresource(
                "certificate",
                "operator",
                &PostParams::default(),
                certificate_request.into(),
            )
            .await?;

        self.certificate.replace(certificate);

        Ok(())
    }
}

impl AsRef<Certificate> for Credentials {
    fn as_ref(&self) -> &Certificate {
        self.certificate.as_ref().expect("Certificate not ready")
    }
}
