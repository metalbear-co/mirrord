#![feature(try_blocks)]
#![warn(clippy::indexing_slicing)]
#![deny(unused_crate_dependencies)]

#[cfg(feature = "client")]
use http::HeaderValue;
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

// TODO(alex) [mid] 6: Move this to /operator/crd/session, probably.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct MirrordCiInfo {
    pub vendor: Option<String>,
    pub target: u64,
    pub branch_name: Option<u64>,
}

#[cfg(feature = "client")]
impl TryFrom<HeaderValue> for MirrordCiInfo {
    type Error = serde_json::Error;

    fn try_from(value: HeaderValue) -> Result<Self, Self::Error> {
        serde_json::from_slice(value.as_bytes())
    }
}
