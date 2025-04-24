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
    pub template: Option<String>,
    pub container: Option<String>,
}

impl FromSplit for WorkflowTarget {
    fn from_split(split: &mut std::str::Split<char>) -> Result<Self> {
        let workflow = split
            .next()
            .ok_or_else(|| ConfigError::InvalidTarget(FAIL_PARSE_DEPLOYMENT_OR_POD.to_string()))?;

        match (split.next(), split.next(), split.next(), split.next()) {
            (Some("template"), Some(template), Some("container"), Some(container)) => Ok(Self {
                workflow: workflow.to_string(),
                template: Some(template.to_string()),
                container: Some(container.to_string()),
            }),
            (Some("template"), Some(template), None, None) => Ok(Self {
                workflow: workflow.to_string(),
                template: Some(template.to_string()),
                container: None,
            }),
            (Some("container"), Some(container), None, None) => Ok(Self {
                workflow: workflow.to_string(),
                template: None,
                container: Some(container.to_string()),
            }),
            (None, None, None, None) => Ok(Self {
                workflow: workflow.to_string(),
                template: None,
                container: None,
            }),
            _ => Err(ConfigError::InvalidTarget(
                FAIL_PARSE_DEPLOYMENT_OR_POD.to_string(),
            )),
        }
    }
}
