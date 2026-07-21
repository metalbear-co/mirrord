use std::{collections::BTreeMap, fmt};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::TargetDisplay;
use crate::config::{ConfigError, Result};

/// Selects every pod in the target namespace that has all configured labels.
#[derive(Serialize, Deserialize, Clone, Eq, PartialEq, Hash, Debug, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LabelTarget {
    /// Exact-match labels used to select pods. All entries must match.
    pub labels: BTreeMap<String, String>,

    /// Container to use in every selected pod.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
}

impl LabelTarget {
    /// Prevents an empty selector from unintentionally targeting every pod in the namespace.
    pub fn verify(&self) -> Result<()> {
        if self.labels.is_empty() {
            return Err(ConfigError::InvalidTarget(
                "Label target must contain at least one label.".to_owned(),
            ));
        }

        Ok(())
    }

    /// Returns an exact-match Kubernetes selector with a deterministic ordering.
    pub fn selector(&self) -> String {
        self.labels
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join(",")
    }
}

impl TargetDisplay for LabelTarget {
    fn type_(&self) -> &str {
        "label"
    }

    fn name(&self) -> &str {
        "label"
    }

    fn container(&self) -> Option<&String> {
        self.container.as_ref()
    }
}

impl fmt::Display for LabelTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "label/{}", self.selector())?;
        if let Some(container) = &self.container {
            write!(f, "/container/{container}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selector_is_deterministic() {
        let target = LabelTarget {
            labels: BTreeMap::from([
                ("team".to_owned(), "payments".to_owned()),
                ("app.kubernetes.io/name".to_owned(), "api".to_owned()),
            ]),
            container: None,
        };

        assert_eq!(
            target.selector(),
            "app.kubernetes.io/name=api,team=payments"
        );
    }

    #[test]
    fn rejects_empty_selector() {
        let target = LabelTarget {
            labels: BTreeMap::new(),
            container: None,
        };

        assert!(target.verify().is_err());
    }
}
