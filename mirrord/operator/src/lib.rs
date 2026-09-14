#![warn(clippy::indexing_slicing)]
#![deny(unused_crate_dependencies)]

#[cfg(feature = "connection")]
use k8s_openapi as _;
#[cfg(test)]
use rstest as _;
#[cfg(test)]
use serde_saphyr as _;
#[cfg(test)]
use tempfile as _;

#[cfg(feature = "client")]
pub mod client;

#[cfg(feature = "connection")]
pub use mirrord_operator_websocket::{connection, upgrade};

#[cfg(feature = "crd")]
pub mod crd;

/// Types used in the operator that don't require any special dependencies
pub mod types;
