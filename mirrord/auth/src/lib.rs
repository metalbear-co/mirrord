#![deny(unused_crate_dependencies)]
#![doc = include_str!("../README.md")]

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

mod deps_required_for_compilation {
    //! To silence false positive from `unused_crate_dependencies`.
    //!
    //! See [discussion on GitHub](https://github.com/rust-lang/cargo/issues/12717#issuecomment-1728123462) for reference.

    #[cfg(test)]
    use k8s_openapi as _;
    use reqwest as _;
}
