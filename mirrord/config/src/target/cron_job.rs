use core::fmt;
use std::fmt::Display;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{FromSplit, FAIL_PARSE_DEPLOYMENT_OR_POD};
use crate::config::{self, ConfigError};

#[derive(Serialize, Deserialize, Clone, Eq, PartialEq, Hash, Debug, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CronJobTarget {
    pub cron_job: String,
    pub container: Option<String>,
}

impl Display for CronJobTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}{}",
            self.container
                .as_ref()
                .map(|c| format!("{c}/"))
                .unwrap_or_default(),
            self.cron_job.clone()
        )
    }
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
