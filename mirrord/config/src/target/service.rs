use std::str::Split;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{FAIL_PARSE_DEPLOYMENT_OR_POD, FromSplit};
use crate::config::{ConfigError, Result};

#[derive(Serialize, Deserialize, Clone, Eq, PartialEq, Hash, Debug, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ServiceTarget {
    pub service: String,
    pub container: Option<String>,
}

impl FromSplit for ServiceTarget {
    fn from_split(split: &mut Split<char>) -> Result<Self> {
        let service = split
            .next()
            .ok_or_else(|| ConfigError::InvalidTarget(FAIL_PARSE_DEPLOYMENT_OR_POD.to_owned()))?;

        match (split.next(), split.next()) {
            (Some("container"), Some(container)) => Ok(Self {
                service: service.to_owned(),
                container: Some(container.to_owned()),
            }),
            (None, None) => Ok(Self {
                service: service.to_owned(),
                container: None,
            }),
            _ => Err(ConfigError::InvalidTarget(
                FAIL_PARSE_DEPLOYMENT_OR_POD.to_owned(),
            )),
        }
    }
}
