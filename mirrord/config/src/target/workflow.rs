use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{FromSplit, TargetDisplay, FAIL_PARSE_DEPLOYMENT_OR_POD};
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
    pub step: Option<String>,
    pub container: Option<String>,
}

impl FromSplit for WorkflowTarget {
    fn from_split(split: &mut std::str::Split<char>) -> Result<Self> {
        let workflow = split
            .next()
            .ok_or_else(|| ConfigError::InvalidTarget(FAIL_PARSE_DEPLOYMENT_OR_POD.to_string()))?;

        let mut target = WorkflowTarget {
            workflow: workflow.to_string(),
            template: None,
            step: None,
            container: None,
        };

        loop {
            match (split.next(), split.next()) {
                (Some("template"), Some(template)) if target.template.is_none() => {
                    target.template = Some(template.to_string());
                }
                (Some("step"), Some(step))
                    if target.template.is_some() && target.step.is_none() =>
                {
                    target.step = Some(step.to_string());
                }
                (Some("container"), Some(container)) => {
                    target.container = Some(container.to_string());
                }
                (None, ..) => break Ok(target),
                _ => {
                    break Err(ConfigError::InvalidTarget(
                        FAIL_PARSE_DEPLOYMENT_OR_POD.to_string(),
                    ))
                }
            }
        }
    }
}

impl fmt::Display for WorkflowTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}/{}{}{}{}",
            self.type_(),
            self.name(),
            self.template
                .as_ref()
                .map(|name| format!("/template/{name}"))
                .unwrap_or_default(),
            self.step
                .as_ref()
                .map(|name| format!("/step/{name}"))
                .unwrap_or_default(),
            self.container
                .as_ref()
                .map(|name| format!("/container/{name}"))
                .unwrap_or_default()
        )
    }
}
