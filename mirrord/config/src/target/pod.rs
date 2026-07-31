use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{FAIL_PARSE_DEPLOYMENT_OR_POD, FromSplit};
use crate::config::{self, ConfigError};

/// <!--${internal}-->
/// Mirror the pod specified by [`PodTarget::pod`].
#[derive(Serialize, Deserialize, Clone, Eq, PartialEq, Hash, Debug, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PodTarget {
    /// <!--${internal}-->
    /// Pod to mirror.
    pub pod: String,
    pub container: Option<String>,
}

impl FromSplit for PodTarget {
    fn from_split(split: &mut std::str::Split<char>) -> config::Result<Self> {
        let pod = split
            .next()
            .ok_or_else(|| ConfigError::InvalidTarget(FAIL_PARSE_DEPLOYMENT_OR_POD.to_owned()))?;
        match (split.next(), split.next()) {
            (Some("container"), Some(container)) => Ok(Self {
                pod: pod.to_owned(),
                container: Some(container.to_owned()),
            }),
            (None, None) => Ok(Self {
                pod: pod.to_owned(),
                container: None,
            }),
            _ => Err(ConfigError::InvalidTarget(
                FAIL_PARSE_DEPLOYMENT_OR_POD.to_owned(),
            )),
        }
    }
}
