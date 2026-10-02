//! Matching TLS client certificates by the identity they carry.
//!
//! When mirrord delivers intercepted mTLS traffic, it connects to the destination server on
//! behalf of the original client. Servers that authorize requests based on the client's identity
//! need mirrord to present a certificate with the same identity. Therefore, mirrord can be
//! configured with multiple client certificates and picks the one matching the original client.

use std::{collections::BTreeSet, ops::Not};

use tracing::Level;
use x509_parser::{
    asn1_rs::{Any, Sequence},
    oid_registry::OID_X509_EXT_SUBJECT_ALT_NAME,
    prelude::{FromDer, X509Certificate},
};

/// Tag of a `dNSName` [GeneralName](https://www.rfc-editor.org/rfc/rfc5280#section-4.2.1.6):
/// context-specific, primitive, number 2.
const DNS_NAME_TAG: u8 = 0x82;

/// Identity of a TLS client, as seen by a server that authorizes requests based on the client's
/// certificate.
///
/// Servers look for the client's identity in the subject alternative names (e.g. a SPIFFE ID in
/// a URI SAN), and in the subject only when there are no SANs (e.g. the common name). Two
/// identities are equal according to the same rule: if they carry SANs, only the SANs are
/// compared, otherwise only the subjects are. This way a certificate re-issued for the same
/// workload still matches, even if its issuer, validity period or subject differ (some issuers
/// put a per-certificate unique identifier in the subject).
///
/// Names are kept DER-encoded, so that the identity can be sent over mirrord-protocol and
/// compared on the other side. DNS names are normalized (ASCII lowercase, no trailing dot), and
/// all other names are compared byte by byte. In particular, there is no RFC 5280 normalization
/// of distinguished names, so subjects with the same attributes stored with different string
/// types (e.g. `PrintableString` and `UTF8String`) do not match.
#[derive(Debug, Clone, Eq)]
pub struct CertIdentity {
    /// DER encoding of the subject's distinguished name.
    pub subject: Vec<u8>,
    /// DER encodings of the subject alternative names (including their tags), sorted and
    /// deduplicated.
    pub subject_alternative_names: Vec<Vec<u8>>,
}

impl PartialEq for CertIdentity {
    fn eq(&self, other: &Self) -> bool {
        self.subject_alternative_names == other.subject_alternative_names
            && (self.subject_alternative_names.is_empty().not() || self.subject == other.subject)
    }
}

impl CertIdentity {
    /// Extracts the identity from the given DER-encoded X509 certificate.
    ///
    /// Returns [`None`] when the certificate cannot be parsed, or carries no identity (empty
    /// subject and no subject alternative names). Certificates without an identity are not
    /// distinguishable from each other, so they must never match.
    #[tracing::instrument(level = Level::DEBUG, skip_all, ret)]
    pub fn from_der(cert: &[u8]) -> Option<Self> {
        let (_, cert) = X509Certificate::from_der(cert)
            .inspect_err(|error| tracing::warn!(%error, "Failed to parse an X509 certificate"))
            .ok()?;

        let mut subject_alternative_names = BTreeSet::new();
        let san_extension = cert
            .get_extension_unique(&OID_X509_EXT_SUBJECT_ALT_NAME)
            .inspect_err(|error| tracing::warn!(%error, "Invalid X509 SAN extension"))
            .ok()?;
        if let Some(extension) = san_extension {
            let (_, names) = Sequence::from_der(extension.value)
                .inspect_err(|error| tracing::warn!(%error, "Invalid X509 SAN extension"))
                .ok()?;
            let mut remaining = names.content.as_ref();
            while remaining.is_empty().not() {
                let (rest, name) = Any::from_der(remaining)
                    .inspect_err(|error| tracing::warn!(%error, "Invalid X509 SAN extension"))
                    .ok()?;
                let (encoded, _) = remaining.split_at(remaining.len() - rest.len());
                let encoded = if encoded.first() == Some(&DNS_NAME_TAG) {
                    normalized_dns_name(name.as_bytes())
                } else {
                    encoded.to_vec()
                };
                subject_alternative_names.insert(encoded);
                remaining = rest;
            }
        }

        if cert.subject().iter_rdn().next().is_none() && subject_alternative_names.is_empty() {
            return None;
        }

        Some(Self {
            subject: cert.subject().as_raw().to_vec(),
            subject_alternative_names: subject_alternative_names.into_iter().collect(),
        })
    }
}

/// Returns the DER encoding of a `dNSName` with the given value, lowercased and stripped of a
/// trailing dot, since DNS names are case-insensitive and `example.com.` is the same name as
/// `example.com`.
fn normalized_dns_name(value: &[u8]) -> Vec<u8> {
    let value = value
        .strip_suffix(b".")
        .unwrap_or(value)
        .to_ascii_lowercase();

    let mut encoded = vec![DNS_NAME_TAG];
    if let Ok(len) = u8::try_from(value.len())
        && len < 0x80
    {
        encoded.push(len);
    } else {
        let len = value.len().to_be_bytes();
        let len = &len[len.iter().take_while(|byte| **byte == 0).count()..];
        encoded.push(0x80 | len.len() as u8);
        encoded.extend_from_slice(len);
    }
    encoded.extend_from_slice(&value);

    encoded
}

#[cfg(test)]
mod test {
    use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, SanType};

    use super::*;
    use crate::generate_cert;

    #[test]
    fn same_identity_from_different_certs() {
        let first = generate_cert("client.example.com", None, false).unwrap();
        let second = generate_cert("client.example.com", None, false).unwrap();
        assert_ne!(first.cert.der(), second.cert.der());

        assert_eq!(
            CertIdentity::from_der(first.cert.der()).unwrap(),
            CertIdentity::from_der(second.cert.der()).unwrap(),
        );
    }

    /// Certificates re-issued for the same client may list the same names in a different order.
    #[test]
    fn san_order_does_not_matter() {
        let make = |names: &[&str]| {
            let names = names
                .iter()
                .map(|name| name.to_string())
                .collect::<Vec<_>>();
            let cert = CertificateParams::new(names)
                .unwrap()
                .self_signed(&KeyPair::generate().unwrap())
                .unwrap();
            CertIdentity::from_der(cert.der()).unwrap()
        };

        assert_eq!(
            make(&["first.example.com", "second.example.com"]),
            make(&["second.example.com", "first.example.com"]),
        );
    }

    /// The same value in SANs of different kinds is a different identity.
    #[test]
    fn san_kind_matters() {
        let make = |san: SanType| {
            let mut params = CertificateParams::default();
            params.distinguished_name = DistinguishedName::new();
            params.subject_alt_names = vec![san];
            let cert = params.self_signed(&KeyPair::generate().unwrap()).unwrap();
            CertIdentity::from_der(cert.der()).unwrap()
        };

        assert_ne!(
            make(SanType::DnsName("client".try_into().unwrap())),
            make(SanType::URI("client".try_into().unwrap())),
        );
    }

    #[test]
    fn different_identities() {
        let first = generate_cert("first.example.com", None, false).unwrap();
        let second = generate_cert("second.example.com", None, false).unwrap();

        assert_ne!(
            CertIdentity::from_der(first.cert.der()).unwrap(),
            CertIdentity::from_der(second.cert.der()).unwrap(),
        );
    }

    /// Workload identity systems (e.g. SPIFFE) often issue certificates with the same subject
    /// for all workloads, and put the identity in a URI SAN.
    #[test]
    fn same_subject_different_uri_san() {
        let make = |spiffe_id: &str| {
            let mut params = CertificateParams::default();
            params.distinguished_name = DistinguishedName::new();
            params
                .distinguished_name
                .push(DnType::OrganizationName, "SPIRE");
            params.subject_alt_names = vec![SanType::URI(spiffe_id.try_into().unwrap())];
            let cert = params.self_signed(&KeyPair::generate().unwrap()).unwrap();
            CertIdentity::from_der(cert.der()).unwrap()
        };

        assert_eq!(
            make("spiffe://cluster.local/ns/default/sa/first"),
            make("spiffe://cluster.local/ns/default/sa/first"),
        );
        assert_ne!(
            make("spiffe://cluster.local/ns/default/sa/first"),
            make("spiffe://cluster.local/ns/default/sa/second"),
        );
    }

    /// When there are SANs, the identity is in the SANs, and the subject may be unique for every
    /// issued certificate.
    #[test]
    fn subject_ignored_with_sans() {
        let make = |common_name: &str| {
            let mut params = CertificateParams::new(vec!["client.example.com".to_owned()]).unwrap();
            params
                .distinguished_name
                .push(DnType::CommonName, common_name);
            let cert = params.self_signed(&KeyPair::generate().unwrap()).unwrap();
            CertIdentity::from_der(cert.der()).unwrap()
        };

        assert_eq!(make("first"), make("second"));
    }

    #[test]
    fn subject_compared_without_sans() {
        let make = |common_name: &str| {
            let mut params = CertificateParams::default();
            params.distinguished_name = DistinguishedName::new();
            params
                .distinguished_name
                .push(DnType::CommonName, common_name);
            let cert = params.self_signed(&KeyPair::generate().unwrap()).unwrap();
            CertIdentity::from_der(cert.der()).unwrap()
        };

        assert_eq!(make("client"), make("client"));
        assert_ne!(make("client"), make("other-client"));
    }

    #[rstest::rstest]
    #[case::case("Client.Example.COM", "client.example.com")]
    #[case::trailing_dot("client.example.com.", "client.example.com")]
    #[case::long_name(&format!("{}.Example.com", "a".repeat(200)), &format!("{}.example.com", "a".repeat(200)))]
    fn dns_names_normalized(#[case] first: &str, #[case] second: &str) {
        let make = |name: &str| {
            let mut params = CertificateParams::default();
            params.distinguished_name = DistinguishedName::new();
            params.subject_alt_names = vec![SanType::DnsName(name.try_into().unwrap())];
            let cert = params.self_signed(&KeyPair::generate().unwrap()).unwrap();
            CertIdentity::from_der(cert.der()).unwrap()
        };

        assert_eq!(make(first), make(second));
    }

    /// Normalized names are valid DER, so that they can be compared with names that did not
    /// need normalization.
    #[rstest::rstest]
    #[case::short(10)]
    #[case::long(200)]
    #[case::very_long(300)]
    fn normalized_dns_name_is_der(#[case] len: usize) {
        let value = "a".repeat(len);
        let encoded = normalized_dns_name(value.as_bytes());

        let (rest, name) = Any::from_der(&encoded).unwrap();
        assert!(rest.is_empty());
        assert_eq!(encoded.first(), Some(&DNS_NAME_TAG));
        assert_eq!(name.as_bytes(), value.as_bytes());
    }

    #[test]
    fn no_identity() {
        let mut params = CertificateParams::default();
        params.distinguished_name = DistinguishedName::new();
        let cert = params.self_signed(&KeyPair::generate().unwrap()).unwrap();
        assert_eq!(CertIdentity::from_der(cert.der()), None);
        assert_eq!(CertIdentity::from_der(b"not a certificate"), None);
    }
}
