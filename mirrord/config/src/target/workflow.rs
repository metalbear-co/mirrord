use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{FromSplit, FAIL_PARSE_DEPLOYMENT_OR_POD};
use crate::config::{ConfigError, Result};

/// <!--${internal}-->
/// Mirror the workflow specified by [`WorkflowTarget::workflow`].
#[derive(Serialize, Deserialize, Clone, Eq, PartialEq, Hash, Debug, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkflowTarget {
    /// <!--${internal}-->
    /// Workflow to mirror.
    pub workflow: String,
    pub container: Option<String>,
}

impl FromSplit for WorkflowTarget {
    fn from_split(split: &mut std::str::Split<char>) -> Result<Self> {
        let workflow = split
            .next()
            .ok_or_else(|| ConfigError::InvalidTarget(FAIL_PARSE_DEPLOYMENT_OR_POD.to_string()))?;
        match (split.next(), split.next()) {
            (Some("container"), Some(container)) => Ok(Self {
                workflow: workflow.to_string(),
                container: Some(container.to_string()),
            }),
            (None, None) => Ok(Self {
                workflow: workflow.to_string(),
                container: None,
            }),
            _ => Err(ConfigError::InvalidTarget(
                FAIL_PARSE_DEPLOYMENT_OR_POD.to_string(),
            )),
        }
    }
}
