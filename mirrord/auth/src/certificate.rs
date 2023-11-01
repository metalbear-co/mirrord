use std::{ops::Deref, str::FromStr};

use chrono::Utc;
use serde::{de, ser, Deserialize, Serialize};
use tracing::warn;
use x509_certificate::{X509Certificate, X509CertificateError};

use crate::credentials::DateValidityExt;

/// Serialize pem contents of `X509Certificate`
fn x509_serialize<S>(certificate: &X509Certificate, serialzer: S) -> Result<S::Ok, S::Error>
where
    S: ser::Serializer,
{
    let certificate = certificate.encode_pem().map_err(ser::Error::custom)?;

    certificate.serialize(serialzer)
}

/// Deserialize `X509Certificate` from pem content
fn x509_deserialize<'de, D>(deserializer: D) -> Result<X509Certificate, D::Error>
where
    D: de::Deserializer<'de>,
{
    let certificate = String::deserialize(deserializer)?;

    X509Certificate::from_pem(certificate).map_err(de::Error::custom)
}

/// Serializable `X509Certificate`
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Certificate(
    #[serde(
        deserialize_with = "x509_deserialize",
        serialize_with = "x509_serialize"
    )]
    X509Certificate,
);

impl Certificate {
    /// Checks this [`Certificate`]'s validty to see if it's expired, and warns if we're close to
    /// expiring, see [`DateValidityExt`].
    ///
    /// Compared with `chrono::Utc::now`.
    pub fn not_expired(&self) -> bool {
        let validity = &self.0.as_ref().tbs_certificate.validity;
        validity.close_to_expiring().inspect(|expires_on| {
            // TODO(alex): Is a warning good enough for this? Should we take a mirrord progress?
            // Is there another way of notifying upstream that it's close, while not adding deps?
            warn!(
                "Operator license will be expiring soon, you have until {} to renew it!",
                expires_on.naive_local()
            )
        });

        self.0
            .as_ref()
            .tbs_certificate
            .validity
            .is_date_valid(Utc::now())
    }
}

impl From<X509Certificate> for Certificate {
    fn from(certificate: X509Certificate) -> Self {
        Certificate(certificate)
    }
}

impl FromStr for Certificate {
    type Err = X509CertificateError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        X509Certificate::from_pem(value).map(Certificate)
    }
}

impl Deref for Certificate {
    type Target = X509Certificate;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
