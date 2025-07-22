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

#[cfg(test)]
mod test {
    use chrono::{TimeZone, Utc};
    use x509_certificate::asn1time::Time;

    use super::Certificate;

    /// Verifies that [`Certificate`] properly deserializes from value produced by old code.
    #[test]
    fn deserialize_from_old_format() {
        const SERIALIZED: &str = r#""-----BEGIN CERTIFICATE-----\r\nMIICGTCCAcmgAwIBAgIBATAHBgMrZXAFADBwMUIwQAYDVQQDDDlUaGUgTWljaGHF\r\ngiBTbW9sYXJlayBPcmdhbml6YXRpb25gcyBUZWFtcyBMaWNlbnNlIChUcmlhbCkx\r\nKjAoBgNVBAoMIVRoZSBNaWNoYcWCIFNtb2xhcmVrIE9yZ2FuaXphdGlvbjAeFw0y\r\nNDAyMDgxNTUwNDFaFw0yNDEyMjQwMDAwMDBaMBsxGTAXBgNVBAMMEHJheno0Nzgw\r\nLW1hY2hpbmUwLDAHBgMrZW4FAAMhAAfxTouyk5L5lB3eFwC5Rg9iI4KmQaFpnGVM\r\n2sYpv9HOo4HYMIHVMIHSBhcvbWV0YWxiZWFyL2xpY2Vuc2UvaW5mbwEB/wSBs3si\r\ndHlwZSI6InRlYW1zIiwibWF4X3NlYXRzIjpudWxsLCJzdWJzY3JpcHRpb25faWQi\r\nOiJmMWIxZDI2ZS02NGQzLTQ4YjYtYjVkMi05MzAxMzAwNWE3MmUiLCJvcmdhbml6\r\nYXRpb25faWQiOiIzNTdhZmE4MS0yN2QxLTQ3YjEtYTFiYS1hYzM1ZjlhM2MyNjMi\r\nLCJ0cmlhbCI6dHJ1ZSwidmVyc2lvbiI6IjMuNzMuMCJ9MAcGAytlcAUAA0EAJbbo\r\nu42KnHJBbPMYspMdv9ZdTQMixJgQUheNEs/o4+XfwgYOaRjCVQTzYs1m9f720WQ9\r\n4J04GdQvcu7B/oTgDQ==\r\n-----END CERTIFICATE-----\r\n""#;
        let cert: Certificate = serde_yaml::from_str(SERIALIZED).unwrap();

        assert_eq!(
            cert.as_ref().signature.octet_bytes().as_ref(),
            b"%\xb6\xe8\xbb\x8d\x8a\x9crAl\xf3\x18\xb2\x93\x1d\xbf\xd6]M\x03\"\xc4\x98\x10R\x17\x8d\x12\xcf\xe8\xe3\xe5\xdf\xc2\x06\x0ei\x18\xc2U\x04\xf3b\xcdf\xf5\xfe\xf6\xd1d=\xe0\x9d8\x19\xd4/r\xee\xc1\xfe\x84\xe0\r",
        );

        assert_eq!(
            cert.as_ref().tbs_certificate.subject_public_key_info.subject_public_key.octet_bytes().as_ref(),
            b"\x07\xf1N\x8b\xb2\x93\x92\xf9\x94\x1d\xde\x17\0\xb9F\x0fb#\x82\xa6A\xa1i\x9ceL\xda\xc6)\xbf\xd1\xce",
        );
        assert_eq!(
            cert.as_ref()
                .tbs_certificate
                .subject
                .user_friendly_str()
                .unwrap(),
            "CN=razz4780-machine",
        );
        assert_eq!(
            cert.as_ref().tbs_certificate.validity.not_before,
            Time::from(Utc.with_ymd_and_hms(2024, 2, 8, 15, 50, 41).unwrap())
        );
        assert_eq!(
            cert.as_ref().tbs_certificate.validity.not_after,
            Time::from(Utc.with_ymd_and_hms(2024, 12, 24, 00, 00, 00).unwrap())
        );
    }
}
