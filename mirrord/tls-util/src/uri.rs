use http::Uri;
use rustls::pki_types::{InvalidDnsNameError, ServerName};

/// Convenience trait for extracting a [`ServerName`] from the request [`Uri`].
pub trait UriExt {
    fn server_name(&self) -> Result<ServerName, InvalidDnsNameError>;
}

impl UriExt for Uri {
    fn server_name(&self) -> Result<ServerName, InvalidDnsNameError> {
        let mut hostname = self.host().unwrap_or_default();

        // Remove square brackets around IPv6 address.
        if let Some(trimmed) = hostname.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
            hostname = trimmed;
        }

        ServerName::try_from(hostname)
    }
}
