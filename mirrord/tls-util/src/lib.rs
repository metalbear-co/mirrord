//! This crate contains various utility structs and trait for logic related to TLS.
//!
//! All code in this struct is blocking, not async, even when it does IO.
//! This is because processing the certificates is computationally heavy anyway, and we should not
//! do this from the main tokio thread. If you need to process TLS certs/keys, best do this in a
//! blocking tokio task.

mod as_pem;
mod cert_chain;
mod cert_with_server_name;
mod certs;
mod error;
mod maybe_mapped_path;
mod no_verifier;
mod random_cert;
mod uri;

pub use as_pem::AsPem;
pub use cert_chain::CertChain;
pub use cert_with_server_name::CertWithServerName;
pub use certs::Certs;
pub use error::TlsUtilError;
pub use maybe_mapped_path::MaybeMappedPath;
pub use no_verifier::{DangerousNoVerifierClient, DangerousNoVerifierServer};
pub use random_cert::RandomCert;
pub use rustls;
pub use tokio_rustls;
pub use uri::UriExt;
