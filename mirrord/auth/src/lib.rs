#![doc = include_str!("../README.md")]
#![deny(unused_crate_dependencies)]

pub use pem;
pub use x509_certificate;

/// Silences `deny(unused_crate_dependencies)`.
/// Although we don't use this dependency directly,
/// compilation fails without it.
#[cfg(feature = "client")]
use k8s_openapi as _;

/// X509 Certificate abstraction for serialization and deserialization
pub mod certificate;
/// FileSystem based storage for multiple credentials (default contents "~/.mirrord/credentials")
#[cfg(feature = "client")]
pub mod credential_store;
/// Credentials used to create from and validate against Operator License
pub mod credentials;
/// Error types
#[cfg(feature = "client")]
pub mod error;
/// Public/Private key abstraction for serialization and deserialization
pub mod key_pair;
