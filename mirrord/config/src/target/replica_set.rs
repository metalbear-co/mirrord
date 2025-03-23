use std::str::Split;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{FromSplit, FAIL_PARSE_DEPLOYMENT_OR_POD};
use crate::config::{ConfigError, Result};

#[derive(Serialize, Deserialize, Clone, Eq, PartialEq, Hash, Debug, JsonSchema)]
#[serde(deny_unknown_fields)]
#[cfg_attr(
    feature = "namespaced-schemars",
    schemars(
        rename = "co.metalbear.mirrord.v1.ReplicaSetTarget",
        title = "ReplicaSetTarget"
    )
)]
pub struct ReplicaSetTarget {
    pub replica_set: String,
    pub container: Option<String>,
}

impl FromSplit for ReplicaSetTarget {
    fn from_split(split: &mut Split<char>) -> Result<Self> {
        let replica_set = split
            .next()
            .ok_or_else(|| ConfigError::InvalidTarget(FAIL_PARSE_DEPLOYMENT_OR_POD.to_string()))?;
        match (split.next(), split.next()) {
            (Some("container"), Some(container)) => Ok(Self {
                replica_set: replica_set.to_string(),
                container: Some(container.to_string()),
            }),
            (None, None) => Ok(Self {
                replica_set: replica_set.to_string(),
                container: None,
            }),
            _ => Err(ConfigError::InvalidTarget(
                FAIL_PARSE_DEPLOYMENT_OR_POD.to_string(),
            )),
        }
    }
}
