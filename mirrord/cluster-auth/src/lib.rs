//! Connections to remote Kubernetes clusters for callers that have no kubeconfig, such as a
//! workload companion running outside the cluster, authenticated with credentials the caller
//! obtains itself.
//!
//! [`ClusterClientFactory`] builds a `kube::Client` from [`ClusterCredentials`]: the API server,
//! its CA, and the [`AuthMethod`]. Bearer tokens are kept in token files that the client
//! reloads at least once per minute, so new requests use refreshed tokens without rebuilding
//! the client. Existing streams remain authenticated for their lifetime. With the `eks`
//! feature, [`ClusterClientFactory::connect_eks`] authenticates as the
//! workload's AWS IAM identity and [`ClusterClientFactory::run_token_refresh`] keeps its token
//! fresh.
//!
//! The code is kept close to the mirrord operator's multi-cluster connections (`operator-envoy`),
//! which are meant to move onto this crate.

mod client;
#[cfg(feature = "eks")]
mod connection;
mod credentials;
pub mod error;
#[cfg(feature = "eks")]
pub mod iam_token;
pub mod jwt;
mod tls;

pub use client::ClusterClientFactory;
#[cfg(feature = "eks")]
pub use connection::ClusterConnection;
pub use credentials::{AuthMethod, ClusterCredentials};
pub use error::{ClusterAuthError, Result};
#[cfg(feature = "eks")]
pub use iam_token::eks_region;
pub use tls::{DefaultTlsBuilder, TlsConfigBuilder};
