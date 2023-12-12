#![feature(let_chains)]
#![feature(lazy_cell)]
#![warn(clippy::indexing_slicing)]

#[cfg(feature = "client")]
pub mod client;

#[cfg(feature = "crd")]
pub mod crd;

/// Operator Setup functinality
#[cfg(feature = "setup")]
pub mod setup;
