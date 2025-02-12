use std::{net::IpAddr, sync::Arc};

use rustls::{
    pki_types::{DnsName, ServerName},
    RootCertStore,
};
use x509_parser::{
    pem,
    prelude::{GeneralName, X509Certificate},
};

use crate::SingleCertRootStoreError;

pub struct SingleCertRootStore {
    store: RootCertStore,
    server_name: ServerName<'static>,
}

impl SingleCertRootStore {
    /// For this method to accept the given `certificate_pem`:
    /// 1. The X509 certificate must be located in the *first* PEM block. Only the *first* PEM block
    ///    is inspected.
    /// 2. The X509 certificate must contain exactly one SAN extension.
    /// 3. The SAN extension must contain at least one SAN that is a DNS name or an IP address. This
    ///    requirement comes from [`TlsConnector::connect`] interface.
    pub fn read(pem: &[u8]) -> Result<Self, SingleCertRootStoreError> {
        let (_, pem) = pem::parse_x509_pem(pem)?;
        let cert = pem.parse_x509()?;
        let server_name = Self::get_san(&cert)?;

        let mut store = RootCertStore::empty();
        store.add(pem.contents.into())?;

        Ok(Self { store, server_name })
    }

    pub fn server_name(&self) -> &ServerName<'static> {
        &self.server_name
    }

    /// Returns the first [`ServerName`] found in the given certificate.
    ///
    /// If the certificate does not contain exactly one SAN extension or the extension does not
    /// contain any SAN that is a DNS name or an IP address, this method returns [`None`].
    fn get_san(
        cert: &X509Certificate<'_>,
    ) -> Result<ServerName<'static>, SingleCertRootStoreError> {
        let extension = cert
            .subject_alternative_name()
            .map_err(SingleCertRootStoreError::InvalidSanExtension)?
            .ok_or(SingleCertRootStoreError::NoSanExtension)?;

        extension
            .value
            .general_names
            .iter()
            .find_map(|general_name| match *general_name {
                GeneralName::DNSName(name) => {
                    let dns_name = DnsName::try_from(name).ok()?.to_owned();

                    Some(ServerName::DnsName(dns_name))
                }

                GeneralName::IPAddress(ip) => {
                    let addr = if let Ok(addr) = <[u8; 4]>::try_from(ip) {
                        IpAddr::from(addr)
                    } else if let Ok(addr) = <[u8; 16]>::try_from(ip) {
                        IpAddr::from(addr)
                    } else {
                        return None;
                    };

                    Some(ServerName::IpAddress(addr.into()))
                }

                _ => None,
            })
            .ok_or(SingleCertRootStoreError::NoSubjectAlternateName)
    }
}

impl From<SingleCertRootStore> for RootCertStore {
    fn from(value: SingleCertRootStore) -> Self {
        value.store
    }
}

impl From<SingleCertRootStore> for Arc<RootCertStore> {
    fn from(value: SingleCertRootStore) -> Self {
        Arc::new(value.store)
    }
}
