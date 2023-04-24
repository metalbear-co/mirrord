#![feature(once_cell)]
#![warn(clippy::indexing_slicing)]

#[cfg(feature = "client")]
pub mod client;

#[cfg(feature = "crd")]
pub mod crd;

pub mod license;

/// Operator Setup functinality
#[cfg(feature = "setup")]
pub mod setup;
