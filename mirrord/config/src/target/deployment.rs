use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{FAIL_PARSE_DEPLOYMENT_OR_POD, FromSplit};
use crate::config::{ConfigError, Result};

/// <!--${internal}-->
/// Mirror the deployment specified by [`DeploymentTarget::deployment`].
#[derive(Serialize, Deserialize, Clone, Eq, PartialEq, Hash, Debug, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeploymentTarget {
    /// <!--${internal}-->
    /// Deployment to mirror.
    pub deployment: String,
    pub container: Option<String>,
}

impl FromSplit for DeploymentTarget {
    fn from_split(split: &mut std::str::Split<char>) -> Result<Self> {
        let deployment = split
            .next()
            .ok_or_else(|| ConfigError::InvalidTarget(FAIL_PARSE_DEPLOYMENT_OR_POD.to_owned()))?;
        match (split.next(), split.next()) {
            (Some("container"), Some(container)) => Ok(Self {
                deployment: deployment.to_owned(),
                container: Some(container.to_owned()),
            }),
            (None, None) => Ok(Self {
                deployment: deployment.to_owned(),
                container: None,
            }),
            _ => Err(ConfigError::InvalidTarget(
                FAIL_PARSE_DEPLOYMENT_OR_POD.to_owned(),
            )),
        }
    }
}
