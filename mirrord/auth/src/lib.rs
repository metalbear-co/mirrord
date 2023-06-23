#![doc = include_str!("../README.md")]
#![feature(result_option_inspect)]
#![feature(lazy_cell)]

pub use pem;
pub use x509_certificate;

/// X509 Certificate abstraction for serialization and deserialization
pub mod certificate;
/// FileSystem based storage for multiple credentials (default contents "~/.mirrord/credentials")
#[cfg(feature = "client")]
pub mod credential_store;
/// Credentials used to create from and validate against Operator License
pub mod credentials;
/// Error types
pub mod error;
/// Public/Private key abstraction for serialization and deserialization
pub mod key_pair;
