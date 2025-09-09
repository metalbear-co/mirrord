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

use std::sync::OnceLock;

pub use kube;

use crate::retry::RetryKube;

pub mod api;
pub mod error;
pub mod resolved;
pub mod retry;

pub static RETRY_KUBE_OPERATIONS_POLICY: OnceLock<RetryKube> = OnceLock::new();
