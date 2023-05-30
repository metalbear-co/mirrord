use std::{ops::Deref, str::FromStr};

use serde::{de, ser, Deserialize, Serialize};
use x509_certificate::{X509Certificate, X509CertificateError};

fn x509_serialize<S>(certificate: &X509Certificate, serialzer: S) -> Result<S::Ok, S::Error>
where
    S: ser::Serializer,
{
    let certificate = certificate.encode_pem().map_err(ser::Error::custom)?;

    certificate.serialize(serialzer)
}

fn x509_deserialize<'de, D>(deserializer: D) -> Result<X509Certificate, D::Error>
where
    D: de::Deserializer<'de>,
{
    let certificate = String::deserialize(deserializer)?;

    X509Certificate::from_pem(certificate).map_err(de::Error::custom)
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Certificate(
    #[serde(
        deserialize_with = "x509_deserialize",
        serialize_with = "x509_serialize"
    )]
    X509Certificate,
);

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
