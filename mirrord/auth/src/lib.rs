#![doc = include_str!("../README.md")]
#![deny(unused_crate_dependencies)]

pub use pem;
pub use x509_certificate;

/// Silences `deny(unused_crate_dependencies)`.
///
/// Although we don't use these dependencies directly, we need them here.
#[cfg(feature = "client")]
mod compilation_deps {
    /// Compilation fails without it.
    use k8s_openapi as _;
    /// We use it with rustls enabled to prevent [`kube`] from using openssl.
    use reqwest as _;
}

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
