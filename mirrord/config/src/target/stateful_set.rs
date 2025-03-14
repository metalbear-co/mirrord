use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{FromSplit, FAIL_PARSE_DEPLOYMENT_OR_POD};
use crate::config::{self, ConfigError};

#[derive(Serialize, Deserialize, Clone, Eq, PartialEq, Hash, Debug, JsonSchema)]
#[serde(deny_unknown_fields)]
#[cfg_attr(
    feature = "namespaced-schemars",
    schemars(rename = "co.metalbear.mirrord.v1.StatefulSetTarget")
)]
pub struct StatefulSetTarget {
    pub stateful_set: String,
    pub container: Option<String>,
}

impl FromSplit for StatefulSetTarget {
    fn from_split(split: &mut std::str::Split<char>) -> config::Result<Self> {
        let stateful_set = split
            .next()
            .ok_or_else(|| ConfigError::InvalidTarget(FAIL_PARSE_DEPLOYMENT_OR_POD.to_string()))?;

        match (split.next(), split.next()) {
            (Some("container"), Some(container)) => Ok(Self {
                stateful_set: stateful_set.to_string(),
                container: Some(container.to_string()),
            }),
            (None, None) => Ok(Self {
                stateful_set: stateful_set.to_string(),
                container: None,
            }),
            _ => Err(ConfigError::InvalidTarget(
                FAIL_PARSE_DEPLOYMENT_OR_POD.to_string(),
            )),
        }
    }
}
