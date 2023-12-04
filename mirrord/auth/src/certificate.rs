use std::{ops::Deref, str::FromStr};

use chrono::{DateTime, Utc};
use serde::{de, ser, Deserialize, Serialize};
use x509_certificate::{X509Certificate, X509CertificateError};

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

/// Serializable [`X509Certificate`]
///
/// Implements [`Deref`] into [`X509Certificate`], so you can take a `&Certificate` and call
/// `.as_ref()` on it to accesses the inner members of [`X509Certificate`], for example:
///
/// ```text
/// pub fn tbs(&self) {
///     self.0
///         .certificate
///         .as_ref()
///         .tbs_certificate; // accessed through the `AsRef` of `X509Certificate`
/// }
/// ```
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Certificate(
    #[serde(
        deserialize_with = "x509_deserialize",
        serialize_with = "x509_serialize"
    )]
    X509Certificate,
);

impl Certificate {
    /// Extracts the expiration date (`not_after`) out of the certificate as a nice
    /// `DateTime<Utc>`.
    pub fn expiration_date(&self) -> DateTime<Utc> {
        let validity = &self.0.as_ref().tbs_certificate.validity;

        match validity.not_after.clone() {
            x509_certificate::asn1time::Time::UtcTime(time) => *time,
            x509_certificate::asn1time::Time::GeneralTime(time) => From::from(time),
        }
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
