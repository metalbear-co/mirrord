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

/// Identity of a TLS client, as seen by a server that authorizes requests based on the client's
/// certificate.
///
/// Consists of the certificate's subject and subject alternative names, which is where servers
/// look for the client's identity (e.g. the common name, or a SPIFFE ID in a URI SAN).
/// Other certificate properties, like the validity period or the issuer, are ignored, so that
/// a certificate re-issued for the same client still matches.
///
/// Names are kept DER-encoded, so that the identity can be sent over mirrord-protocol and
/// compared on the other side without depending on how names are formatted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertIdentity {
    /// DER encoding of the subject's distinguished name.
    pub subject: Vec<u8>,
    /// DER encodings of the subject alternative names (including their tags).
    pub subject_alternative_names: Vec<Vec<u8>>,
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
                let (rest, _) = Any::from_der(remaining)
                    .inspect_err(|error| tracing::warn!(%error, "Invalid X509 SAN extension"))
                    .ok()?;
                let (name, _) = remaining.split_at(remaining.len() - rest.len());
                subject_alternative_names.insert(name.to_vec());
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

    #[test]
    fn no_identity() {
        let mut params = CertificateParams::default();
        params.distinguished_name = DistinguishedName::new();
        let cert = params.self_signed(&KeyPair::generate().unwrap()).unwrap();
        assert_eq!(CertIdentity::from_der(cert.der()), None);
        assert_eq!(CertIdentity::from_der(b"not a certificate"), None);
    }
}
