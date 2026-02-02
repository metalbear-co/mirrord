#![feature(try_blocks)]
#![warn(clippy::indexing_slicing)]
#![deny(unused_crate_dependencies)]

#[cfg(feature = "client")]
pub mod client;

#[cfg(feature = "crd")]
pub mod crd;

/// Types used in the operator that don't require any special dependencies
pub mod types;

// small patch to hide `unused_crate_dependencies` error when "crd" featrue isn't selected
#[cfg(all(test, not(feature = "crd")))]
use rstest as _;
