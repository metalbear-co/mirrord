#![feature(try_trait_v2)]
#![warn(clippy::indexing_slicing)]
#![deny(unused_crate_dependencies)]
// TODO(alex): Get a big `Box` for the big variants.
#![allow(clippy::large_enum_variant)]

//! # Features
//!
//! ## `incluster`
//!
//! Turn this feature on if you want to connect to agent pods from within the cluster with a plain
//! TCP connection.
//!
//! ## `portforward`
//!
//! Turn this feature on if you want to connect to agent pods from outside the cluster with port
//! forwarding.
//!
//! ## `re-export-kube`
//!
//! Turn this feature on if you want to use a re-exported kube from this library instead of
//! declaring it as its own dependency. This helps you always keep the kube version matching to this
//! library's kube version, which is necessary because we use types from kube in our public API.

#[cfg(feature = "re-export-kube")]
pub use kube;

pub mod api;
pub mod error;
pub mod resolved;
