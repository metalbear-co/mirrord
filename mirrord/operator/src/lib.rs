#![warn(clippy::indexing_slicing)]
#![deny(unused_crate_dependencies)]

#[cfg(test)]
use rstest as _;
#[cfg(test)]
use serde_saphyr as _;
#[cfg(test)]
use tempfile as _;

#[cfg(feature = "client")]
pub mod client;

#[cfg(feature = "crd")]
pub mod crd;

#[cfg(feature = "crd")]
pub mod preview_template;

/// Types used in the operator that don't require any special dependencies
pub mod types;
