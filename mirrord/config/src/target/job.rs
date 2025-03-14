use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{FromSplit, FAIL_PARSE_DEPLOYMENT_OR_POD};
use crate::config::{self, ConfigError};

#[derive(Serialize, Deserialize, Clone, Eq, PartialEq, Hash, Debug, JsonSchema)]
#[serde(deny_unknown_fields)]
#[cfg_attr(
    feature = "namespaced-schemars",
    schemars(rename = "co.metalbear.mirrord.v1.JobTarget")
)]
pub struct JobTarget {
    pub job: String,
    pub container: Option<String>,
}

impl FromSplit for JobTarget {
    fn from_split(split: &mut std::str::Split<char>) -> config::Result<Self> {
        let job = split
            .next()
            .ok_or_else(|| ConfigError::InvalidTarget(FAIL_PARSE_DEPLOYMENT_OR_POD.to_string()))?;

        match (split.next(), split.next()) {
            (Some("container"), Some(container)) => Ok(Self {
                job: job.to_string(),
                container: Some(container.to_string()),
            }),
            (None, None) => Ok(Self {
                job: job.to_string(),
                container: None,
            }),
            _ => Err(ConfigError::InvalidTarget(
                FAIL_PARSE_DEPLOYMENT_OR_POD.to_string(),
            )),
        }
    }
}
