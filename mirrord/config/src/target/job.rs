use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{FromSplit, FAIL_PARSE_DEPLOYMENT_OR_POD};
use crate::config::{self, ConfigError};

#[derive(Serialize, Deserialize, Clone, Eq, PartialEq, Hash, Debug, JsonSchema)]
#[serde(deny_unknown_fields)]
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

impl core::fmt::Display for JobTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}{}",
            self.container
                .as_ref()
                .map(|c| format!("{c}/"))
                .unwrap_or_default(),
            self.job.clone()
        )
    }
}
