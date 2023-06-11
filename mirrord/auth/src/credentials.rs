use std::{fmt::Debug, ops::Deref};

use chrono::{DateTime, Utc};
use kube::{api::PostParams, Api, Client, Resource};
use serde::{Deserialize, Serialize};
pub use x509_certificate;
use x509_certificate::{
    asn1time::Time, rfc2986, rfc5280, InMemorySigningKeyPair, KeyAlgorithm, X509CertificateBuilder,
    X509CertificateError,
};

use crate::{
    certificate::Certificate,
    error::{AuthenticationError, Result},
    key_pair::KeyPair,
};

#[derive(Debug, Serialize, Deserialize)]
pub struct Credentials {
    /// Generated Certificate from operator can be None when request isn't signed yet and will
    /// result in is_ready returning false
    certificate: Option<Certificate>,
    /// Local Keypair for creating CertificateRequest for operator
    key_pair: KeyPair,
}

impl Credentials {
    /// Creatate new credentials with random `key_pair` and empty `certificate`
    pub fn init() -> Result<Self> {
        let key_algorithm = KeyAlgorithm::Ed25519;
        let (_, document) = InMemorySigningKeyPair::generate_random(key_algorithm)?;

        let pem_key = pem::Pem::new("PRIVATE KEY", document.as_ref());

        Ok(Credentials {
            certificate: None,
            key_pair: pem::encode(&pem_key).into(),
        })
    }

    /// Checks if certificate exists in credentials and the validitiy in terms of expiration
    pub fn is_ready(&self) -> bool {
        let Some(certificate) = self.certificate.as_ref() else {
            return false
        };

        certificate
            .as_ref()
            .tbs_certificate
            .validity
            .is_date_valid(Utc::now())
    }

    /// Create `rfc2986::CertificationRequest` for `Certificate` generation signed with `KeyPair`
    pub fn certificate_request(&self, common_name: &str) -> Result<rfc2986::CertificationRequest> {
        let mut builder = X509CertificateBuilder::new(KeyAlgorithm::Ed25519);

        let _ = builder
            .subject()
            .append_common_name_utf8_string(common_name);

        builder
            .create_certificate_signing_request(self.key_pair.deref())
            .map_err(AuthenticationError::from)
    }

    /// Create `rfc2986::CertificationRequest` and send to operator to replace `Certificate` inside
    /// of self.certificate making the Credentials ready
    pub async fn get_client_certificate<R>(
        &mut self,
        client: Client,
        common_name: &str,
    ) -> Result<()>
    where
        R: Resource + Clone + Debug,
        R: for<'de> Deserialize<'de>,
        R::DynamicType: Default,
    {
        let certificate_request = self
            .certificate_request(common_name)?
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

pub trait DateValidityExt {
    fn is_date_valid(&self, other: DateTime<Utc>) -> bool;
}

impl DateValidityExt for rfc5280::Validity {
    fn is_date_valid(&self, other: DateTime<Utc>) -> bool {
        let not_before: DateTime<Utc> = match self.not_before.clone() {
            Time::UtcTime(time) => *time,
            Time::GeneralTime(time) => DateTime::<Utc>::from(time),
        };

        let not_after: DateTime<Utc> = match self.not_after.clone() {
            Time::UtcTime(time) => *time,
            Time::GeneralTime(time) => DateTime::<Utc>::from(time),
        };

        not_before < other && other < not_after
    }
}
