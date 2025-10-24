#![feature(try_blocks)]
#![warn(clippy::indexing_slicing)]
#![deny(unused_crate_dependencies)]

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[cfg(feature = "client")]
pub mod client;

#[cfg(feature = "crd")]
pub mod crd;

/// Operator Setup functionality
#[cfg(feature = "setup")]
pub mod setup;

/// Types used in the operator that don't require any special dependencies
pub mod types;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub enum CiTrigger {
    Pr,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MirrordCiInfo {
    pub vendor: Option<String>,
    pub target: u64,
    pub branch_name: Option<u64>,
}
