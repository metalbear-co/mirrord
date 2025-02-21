//! This crate contains common utils for handling TLS setup.
//!
//! # Processing PEM files
//!
//! Util functions for processing PEM files (e.g building a root certificate store from multiple
//! files) always use blocking tokio tasks internally.
//!
//! This is because:
//! 1. Apart from the IO, processing the found crypto items is not trivial and may be
//!    computationally heavy.
//! 2. Each [`tokio::fs`] operation spawns a blocking task anyway.
//!
//! Using blocking tasks inside these functions makes this crate safe and easy to use in async code.

mod error;
mod no_verifier;
mod read_pem;
mod root_store;
mod san;
mod uri_ext;

pub use error::FromPemError;
pub use no_verifier::{DangerousNoVerifierClient, DangerousNoVerifierServer};
pub use read_pem::{read_cert_chain, read_key_der};
pub use root_store::best_effort_root_store;
pub use san::HasSubjectAlternateNames;
pub use uri_ext::UriExt;
