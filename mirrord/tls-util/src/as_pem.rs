use pem::{EncodeConfig, LineEnding, Pem};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

/// Convenience trait for producing PEM-encoded representations of TLS structs.
pub trait AsPem {
    fn as_pem(&self) -> String;
}

impl AsPem for CertificateDer<'_> {
    fn as_pem(&self) -> String {
        pem::encode_config(
            &Pem::new("CERTIFICATE", self.to_vec()),
            EncodeConfig::new().set_line_ending(LineEnding::LF),
        )
    }
}

impl AsPem for PrivateKeyDer<'_> {
    fn as_pem(&self) -> String {
        let tag = match self {
            Self::Pkcs1(..) => "RSA PRIVATE KEY",
            Self::Pkcs8(..) => "PRIVATE KEY",
            Self::Sec1(..) => "EC PRIVATE KEY",
            _ => panic!("unsupported key type"),
        };

        pem::encode_config(
            &Pem::new(tag, self.secret_der()),
            EncodeConfig::new().set_line_ending(LineEnding::LF),
        )
    }
}
