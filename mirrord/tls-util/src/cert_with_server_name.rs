use std::{fmt, fs, net::IpAddr};

use rustls::pki_types::{CertificateDer, DnsName, ServerName};
use x509_parser::{
    pem,
    prelude::{GeneralName, X509Certificate},
};

use crate::{NicePath, TlsUtilError};

pub struct CertWithServerName {
    cert: CertificateDer<'static>,
    server_name: ServerName<'static>,
}

impl fmt::Debug for CertWithServerName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CertWithServerName")
            .field("server_name", &self.server_name)
            .finish()
    }
}

impl CertWithServerName {
    /// For this method to accept the given `certificate_pem`:
    /// 1. The X509 certificate must be located in the *first* PEM block. Only the *first* PEM block
    ///    is inspected.
    /// 2. The X509 certificate must contain exactly one SAN extension.
    /// 3. The SAN extension must contain at least one SAN that is a DNS name or an IP address. This
    ///    requirement comes from [`TlsConnector::connect`] interface.
    pub fn parse(pem: &[u8]) -> Result<Self, TlsUtilError> {
        let (_, pem) = pem::parse_x509_pem(pem)?;
        let cert = pem.parse_x509()?;
        let server_name = Self::get_san(&cert)?;

        Ok(Self {
            cert: pem.contents.into(),
            server_name,
        })
    }

    pub fn read<P: NicePath + ?Sized>(path: &P) -> Result<Self, TlsUtilError> {
        let data = fs::read(path.real_path()).map_err(|error| TlsUtilError::ParsePemFileError {
            error,
            path: path.display_path().to_path_buf(),
        })?;

        Self::parse(&data)
    }

    pub fn server_name(&self) -> &ServerName<'static> {
        &self.server_name
    }

    /// Returns the first [`ServerName`] found in the given certificate.
    ///
    /// If the certificate does not contain exactly one SAN extension or the extension does not
    /// contain any SAN that is a DNS name or an IP address, this method returns [`None`].
    fn get_san(cert: &X509Certificate<'_>) -> Result<ServerName<'static>, TlsUtilError> {
        let extension = cert
            .subject_alternative_name()
            .map_err(TlsUtilError::InvalidSanExtension)?
            .ok_or(TlsUtilError::NoSanExtension)?;

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
            .ok_or(TlsUtilError::NoSubjectAlternateName)
    }
}

impl From<CertWithServerName> for CertificateDer<'static> {
    fn from(value: CertWithServerName) -> Self {
        value.cert
    }
}

#[cfg(test)]
mod test {
    use rustls::pki_types::ServerName;

    use super::CertWithServerName;
    use crate::{AsPem, RandomCert};

    #[test]
    fn generate_random_and_parse() {
        let random = RandomCert::generate(vec!["operator".into()]).unwrap();
        let as_pem = random.cert().as_pem();
        let parsed = CertWithServerName::parse(as_pem.as_bytes()).unwrap();
        assert_eq!(
            *parsed.server_name(),
            ServerName::try_from("operator").unwrap()
        );
    }
}
