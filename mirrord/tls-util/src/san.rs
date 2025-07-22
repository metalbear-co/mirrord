use std::net::IpAddr;

use rustls::pki_types::{CertificateDer, ServerName};
use tracing::Level;
use x509_parser::{
    pem::Pem,
    prelude::{FromDer, GeneralName, X509Certificate},
};

use crate::error::GetSanError;

/// Convenience trait for extracting [`ServerName`]s from an X509 certificate.
pub trait HasSubjectAlternateNames {
    fn subject_alternate_names(&self) -> Result<Vec<ServerName<'static>>, GetSanError>;
}

impl HasSubjectAlternateNames for CertificateDer<'_> {
    #[tracing::instrument(level = Level::DEBUG, skip(self), ret, err(level = Level::DEBUG))]
    fn subject_alternate_names(&self) -> Result<Vec<ServerName<'static>>, GetSanError> {
        X509Certificate::from_der(self)?.1.subject_alternate_names()
    }
}

impl HasSubjectAlternateNames for Pem {
    #[tracing::instrument(level = Level::DEBUG, skip(self), ret, err(level = Level::DEBUG))]
    fn subject_alternate_names(&self) -> Result<Vec<ServerName<'static>>, GetSanError> {
        self.parse_x509()?.subject_alternate_names()
    }
}

impl HasSubjectAlternateNames for X509Certificate<'_> {
    #[tracing::instrument(level = Level::DEBUG, skip(self), ret, err(level = Level::DEBUG))]
    fn subject_alternate_names(&self) -> Result<Vec<ServerName<'static>>, GetSanError> {
        let extension = self
            .subject_alternative_name()
            .map_err(GetSanError::InvalidSanExtension)?
            .ok_or(GetSanError::NoSanExtension)?;

        let names = extension
            .value
            .general_names
            .iter()
            .filter_map(|general_name| {
                match *general_name {
            GeneralName::DNSName(name) => {
                ServerName::try_from(name)
                    .inspect_err(|error| {
                        tracing::warn!(%error, name, "SAN extension contains an invalid DNS name")
                    })
                    .ok()?
                    .to_owned()
                    .into()
            }

            GeneralName::IPAddress(ip) => {
                let addr = if let Ok(addr) = <[u8; 4]>::try_from(ip) {
                    IpAddr::from(addr)
                } else if let Ok(addr) = <[u8; 16]>::try_from(ip) {
                    IpAddr::from(addr)
                } else {
                    tracing::error!(?ip, "SAN extension contains an invalid IP address");
                    return None;
                };

                Some(ServerName::from(addr))
            }

            _ => None,
        }
            })
            .collect();

        Ok(names)
    }
}
