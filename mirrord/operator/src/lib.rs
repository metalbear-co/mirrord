#![feature(let_chains)]
#![feature(lazy_cell)]
#![feature(try_blocks)]
#![warn(clippy::indexing_slicing)]

#[cfg(feature = "client")]
pub mod client;

#[cfg(feature = "crd")]
pub mod crd;

/// Operator Setup functionality
#[cfg(feature = "setup")]
pub mod setup;

/// Types used in the operator that don't require any special dependencies
pub mod types;
