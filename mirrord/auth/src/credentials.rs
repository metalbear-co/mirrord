use std::{fmt::Debug, ops::Deref};

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
pub use x509_certificate;
use x509_certificate::{
    asn1time::Time, rfc2986, rfc5280, InMemorySigningKeyPair, KeyAlgorithm, X509CertificateBuilder,
};

use crate::{
    certificate::Certificate,
    error::{AuthenticationError, Result},
    key_pair::KeyPair,
};

/// Client Credentials container for authorization against Operator
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
            return false;
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
}

impl AsRef<Certificate> for Credentials {
    fn as_ref(&self) -> &Certificate {
        self.certificate.as_ref().expect("Certificate not ready")
    }
}

/// Extends a date type ([`DateTime<Utc>`]) to help us when checking for a license's
/// certificate validity.
pub trait LicenseValidity {
    /// How many days we consider a license is close to expiring.
    ///
    /// You can access this constant as
    /// `<DateTime<Utc> as LicenseValidity>::CLOSE_TO_EXPIRATION_DAYS`.
    const CLOSE_TO_EXPIRATION_DAYS: u64 = 2;

    /// This date's validity is good.
    fn is_good(&self) -> bool;

    /// How many days until expiration from this date counting from _now_, which means that an
    /// expiration date of `today + 3` means we have 2 days left until expiry.
    fn days_until_expiration(&self) -> Option<u64>;

    /// Converts a [`NaiveDate`] into a [`NaiveDateTime`], so we can turn it into a
    /// [`DateTime<UTC>`] to check a license's validity.
    ///
    /// I(alex) think this might cause trouble with potential mismatched timezone offsets, but
    /// this is used only for a warning to the user.
    fn from_naive_date(naive_date: NaiveDate) -> DateTime<Utc> {
        let now = Utc::now();
        let offset = *now.offset();
        DateTime::<Utc>::from_naive_utc_and_offset(
            naive_date
                .and_time(NaiveTime::from_hms_opt(0, 0, 0).expect("Manually building valid date!")),
            offset,
        )
    }
}

impl LicenseValidity for DateTime<Utc> {
    fn is_good(&self) -> bool {
        let now = Utc::now();
        now < *self
    }

    fn days_until_expiration(&self) -> Option<u64> {
        if self.is_good() {
            let until_expiration = (*self - Utc::now()).num_days();

            // We only want to return `Some(>= 0)`, never any negative numbers.
            (0..=2)
                .contains(&until_expiration)
                .then_some(until_expiration as u64)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Days, Utc};

    use crate::credentials::LicenseValidity;

    #[test]
    fn license_validity_valid() {
        let today: DateTime<Utc> = Utc::now();
        let expiration_date = today.checked_add_days(Days::new(7)).unwrap();

        assert!(expiration_date.is_good());
    }

    #[test]
    fn license_validity_expired() {
        let today: DateTime<Utc> = Utc::now();
        let expiration_date = today.checked_sub_days(Days::new(7)).unwrap();

        assert!(!expiration_date.is_good());
    }

    #[test]
    fn license_validity_close_to_expiring() {
        let today: DateTime<Utc> = Utc::now();
        let expiration_date = today.checked_add_days(Days::new(3)).unwrap();

        assert_eq!(expiration_date.days_until_expiration(), Some(2));
    }
}

/// Ext trait for validation of dates of `rfc5280::Validity`
pub trait DateValidityExt {
    /// Check other is in between not_before and not_after
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

/// Extenstion of Credentials for functions that accesses Operator
#[cfg(feature = "client")]
pub mod client {
    use kube::{api::PostParams, Api, Client, Resource};

    use super::*;

    impl Credentials {
        /// Create `rfc2986::CertificationRequest` and send to operator to replace `Certificate`
        /// inside of self.certificate making the Credentials ready
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
                .map_err(x509_certificate::X509CertificateError::from)?;

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
}
