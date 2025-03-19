use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{FromSplit, FAIL_PARSE_DEPLOYMENT_OR_POD};
use crate::config::{self, ConfigError};

#[derive(Serialize, Deserialize, Clone, Eq, PartialEq, Hash, Debug, JsonSchema)]
#[serde(deny_unknown_fields)]
#[cfg_attr(
    feature = "namespaced-schemars",
    schemars(rename = "co.metalbear.mirrord.v1.CronJobTarget")
)]
pub struct CronJobTarget {
    pub cron_job: String,
    pub container: Option<String>,
}

impl FromSplit for CronJobTarget {
    fn from_split(split: &mut std::str::Split<char>) -> config::Result<Self> {
        let cron_job = split
            .next()
            .ok_or_else(|| ConfigError::InvalidTarget(FAIL_PARSE_DEPLOYMENT_OR_POD.to_string()))?;

        match (split.next(), split.next()) {
            (Some("container"), Some(container)) => Ok(Self {
                cron_job: cron_job.to_string(),
                container: Some(container.to_string()),
            }),
            (None, None) => Ok(Self {
                cron_job: cron_job.to_string(),
                container: None,
            }),
            _ => Err(ConfigError::InvalidTarget(
                FAIL_PARSE_DEPLOYMENT_OR_POD.to_string(),
            )),
        }
    }
}
