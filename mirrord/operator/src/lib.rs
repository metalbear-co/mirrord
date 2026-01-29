#![feature(try_blocks)]
#![warn(clippy::indexing_slicing)]
#![deny(unused_crate_dependencies)]

#[cfg(feature = "client")]
pub mod client;

#[cfg(feature = "crd")]
pub mod crd;

/// Types used in the operator that don't require any special dependencies
pub mod types;
