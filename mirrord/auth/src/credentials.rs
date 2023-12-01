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

/// Gives some more meaning to a `License`'s [`Certificate`] expiration date, so we don't have
/// to manually check dates to see if the licese is still good or not.
///
/// You may use the [`From`] implementation to build a [`LicenseValidity`] from a [`NaiveDate`],
/// which is what we store in the `LicenseInfoOwned` type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LicenseValidity {
    /// The license is still far away from expiring (see `LicenseValidity::new`).
    ///
    /// Holds the expiration date of the license.
    Good(DateTime<Utc>),

    /// License has expired.
    ///
    /// Holds the expiration date of the license.
    Expired(DateTime<Utc>),
}

impl LicenseValidity {
    /// Builds the [`LicenseValidity`]. You should use either this or the [`From`] implementation.
    ///
    /// We take `now` as a parameter here so we can have [`LicenseValidity`] with fake values.
    pub fn new(expiration_date: DateTime<Utc>, now: DateTime<Utc>) -> Self {
        if now > expiration_date {
            Self::Expired(expiration_date)
        } else {
            Self::Good(expiration_date)
        }
    }

    /// Checks if this [`LicenseValidity`] represents a date that is close to expiring.
    ///
    /// We consider close if the `License` is 2 days away from its `expiration_date`.
    pub fn close_to_expiring(&self) -> Option<i64> {
        match self {
            LicenseValidity::Good(expiration_date) => {
                let until_expiration = (Utc::now() - expiration_date).num_days();

                if until_expiration <= 2 {
                    Some(until_expiration)
                } else {
                    None
                }
            }
            LicenseValidity::Expired(_) => Some(-1),
        }
    }
}

impl From<NaiveDate> for LicenseValidity {
    /// Converts a [`NaiveDate`] into a [`NaiveDateTime`], so we can turn it into a
    /// [`DateTime<UTC>`] and build a nice [`LicenseValidity`].
    ///
    /// I(alex) think this might cause trouble with potential mismatched timezone offsets, but
    /// this is used only for a warning to the user.
    fn from(value: NaiveDate) -> Self {
        let now = Utc::now();
        let offset = now.offset().clone();
        let expiration_date = DateTime::<Utc>::from_naive_utc_and_offset(
            value.and_time(NaiveTime::from_hms_opt(0, 0, 0).unwrap()),
            offset,
        );

        Self::new(expiration_date, now)
    }
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Days, Utc};

    use crate::credentials::LicenseValidity;

    const TODAY: &str = "2023-11-20 00:00:00 UTC";

    #[test]
    fn license_validity_valid() {
        let today: DateTime<Utc> = TODAY.parse().unwrap();
        let expiration_date = today.checked_add_days(Days::new(7)).unwrap();

        assert!(matches!(
            LicenseValidity::new(expiration_date, today),
            LicenseValidity::Good(_)
        ));
    }

    #[test]
    fn license_validity_expired() {
        let today: DateTime<Utc> = TODAY.parse().unwrap();

        let fake_today = today.checked_add_days(Days::new(7)).unwrap();
        let expiration_date = today.checked_add_days(Days::new(7)).unwrap();

        assert!(matches!(
            LicenseValidity::new(expiration_date, fake_today),
            LicenseValidity::Expired(_)
        ));
    }

    #[test]
    fn license_validity_expired_past_expiration_date() {
        let today: DateTime<Utc> = TODAY.parse().unwrap();

        let fake_today = today.checked_add_days(Days::new(8)).unwrap();
        let expiration_date = today.checked_add_days(Days::new(7)).unwrap();

        assert!(matches!(
            LicenseValidity::new(expiration_date, fake_today),
            LicenseValidity::Expired(_)
        ));
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
