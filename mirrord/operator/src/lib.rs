#![feature(try_blocks)]
#![warn(clippy::indexing_slicing)]
#![deny(unused_crate_dependencies)]

use http::{HeaderMap, HeaderValue};
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

impl From<&MirrordCiInfo> for HeaderMap {
    fn from(
        MirrordCiInfo {
            vendor,
            target,
            branch_name,
        }: &MirrordCiInfo,
    ) -> Self {
        let mut headers = HeaderMap::new();

        if let Some(vendor) = vendor {
            headers.insert("x-ci-info-vendor", HeaderValue::from_str(vendor).unwrap());
        }

        if let Some(branch_name) = branch_name {
            headers.insert(
                "x-ci-branch-name",
                HeaderValue::try_from(branch_name.to_string()).unwrap(),
            );
        }

        headers.insert(
            "x-ci-target",
            HeaderValue::try_from(target.to_string()).unwrap(),
        );

        headers
    }
}
