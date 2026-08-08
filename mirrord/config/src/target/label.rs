use std::{collections::BTreeMap, fmt, ops::Not, str::FromStr};

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

impl FromStr for LabelTarget {
    type Err = ConfigError;

    fn from_str(target: &str) -> Result<Self> {
        let path = target
            .strip_prefix("label/")
            .ok_or_else(|| invalid_path(target))?;
        let (selector, container) = match path.rsplit_once("/container/") {
            Some((_, container)) if container.is_empty() || container.contains('/') => {
                return Err(invalid_path(target));
            }
            Some((selector, container)) => (selector, Some(container.to_owned())),
            None => (path, None),
        };

        let mut labels = BTreeMap::new();
        if selector.is_empty().not() {
            for requirement in selector.split(',') {
                let (key, value) = requirement
                    .split_once('=')
                    .filter(|(key, _)| key.is_empty().not())
                    .ok_or_else(|| invalid_path(target))?;

                if labels.insert(key.to_owned(), value.to_owned()).is_some() {
                    return Err(ConfigError::InvalidTarget(format!(
                        "Label target contains duplicate key `{key}`."
                    )));
                }
            }
        }

        let target = Self { labels, container };
        target.verify()?;
        Ok(target)
    }
}

fn invalid_path(target: &str) -> ConfigError {
    ConfigError::InvalidTarget(format!(
        "Label target `{target}` must use `label/key=value[,key=value...][/container/name]`."
    ))
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
    use rstest::rstest;

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

    #[rstest]
    #[case::one_label_with_container(
        "label/app=biskupin/container/zamek-w-besiekierach",
        &[("app", "biskupin")],
        Some("zamek-w-besiekierach"),
    )]
    #[case::one_label_without_container(
        "label/app=biskupin",
        &[("app", "biskupin")],
        None,
    )]
    #[case::two_labels_with_container(
        "label/app=biskupin,tier=web/container/zamek-w-besiekierach",
        &[("app", "biskupin"), ("tier", "web")],
        Some("zamek-w-besiekierach"),
    )]
    #[case::two_labels_without_container(
        "label/app=biskupin,tier=web",
        &[("app", "biskupin"), ("tier", "web")],
        None,
    )]
    #[case::three_labels_with_container(
        "label/app.kubernetes.io/name=biskupin,castle=zamek-w-besiekierach,tier=web/container/php",
        &[
            ("app.kubernetes.io/name", "biskupin"),
            ("castle", "zamek-w-besiekierach"),
            ("tier", "web"),
        ],
        Some("php"),
    )]
    fn parses_label_target_path(
        #[case] input: &str,
        #[case] labels: &[(&str, &str)],
        #[case] container: Option<&str>,
    ) {
        let target = input.parse::<LabelTarget>().unwrap();
        let expected_labels = labels
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();

        assert_eq!(target.labels, expected_labels);
        assert_eq!(target.container.as_deref(), container);
        assert_eq!(target.to_string(), input);
    }

    #[test]
    fn rejects_duplicate_label_keys() {
        assert!(matches!(
            "label/app=biskupin,app=zamek-w-besiekierach".parse::<LabelTarget>(),
            Err(ConfigError::InvalidTarget(message))
                if message == "Label target contains duplicate key `app`."
        ));
    }
}
