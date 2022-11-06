use std::str::FromStr;

use schemars::JsonSchema;
use serde::Deserialize;

use crate::{
    config::{from_env::FromEnv, source::MirrordConfigSource, ConfigError, MirrordConfig, Result},
    util::string_or_struct_option,
};

/// Specifies the target to mirror. See [`Target`].
///
/// ## Examples
///
/// - Mirror pod `hello-world-abcd-1234` in the `hello` namespace:
///
/// ```toml
/// # mirrord-config.toml
///
/// [target]
/// path = "pod/hello-world-abcd-1234"
/// namespace = "hello"
/// ```
#[derive(Deserialize, PartialEq, Eq, Clone, Debug, JsonSchema)]
#[serde(untagged, rename_all = "lowercase")]
pub enum TargetFileConfig {
    Simple(#[serde(deserialize_with = "string_or_struct_option")] Option<Target>),
    Advanced {
        #[serde(deserialize_with = "string_or_struct_option")]
        path: Option<Target>,
        namespace: Option<String>,
    },
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TargetConfig {
    pub path: Option<Target>,
    pub namespace: Option<String>,
}

impl Default for TargetFileConfig {
    fn default() -> Self {
        TargetFileConfig::Simple(None)
    }
}

impl MirrordConfig for TargetFileConfig {
    type Generated = TargetConfig;

    fn generate_config(self) -> Result<Self::Generated> {
        let config = match self {
            TargetFileConfig::Simple(path) => TargetConfig {
                path: (FromEnv::new("MIRRORD_IMPERSONATED_TARGET"), path).source_value(),
                namespace: FromEnv::new("MIRRORD_TARGET_NAMESPACE").source_value(),
            },
            TargetFileConfig::Advanced { path, namespace } => TargetConfig {
                path: (FromEnv::new("MIRRORD_IMPERSONATED_TARGET"), path).source_value(),
                namespace: (FromEnv::new("MIRRORD_TARGET_NAMESPACE"), namespace).source_value(),
            },
        };

        Ok(config)
    }
}

trait FromSplit {
    fn from_split(split: &mut std::str::Split<char>) -> Result<Self>
    where
        Self: Sized;
}

const FAIL_PARSE_DEPLOYMENT_OR_POD: &str = r#"
mirrord-layer failed to parse the provided target!

- Valid format:
    >> deployment/<deployment-name>[/container/container-name]
    >> deploy/<deployment-name>[/container/container-name]
    >> pod/<pod-name>[/container/container-name]

- Note:
    >> specifying container name is optional, defaults to the first container in the provided pod/deployment target.
    >> specifying the pod name is optional, defaults to the first pod in case the target is a deployment.

- Suggestions:
    >> check for typos in the provided target.
    >> check if the provided target exists in the cluster using `kubectl get/describe` commands.
    >> check if the provided target is in the correct namespace.
"#;

/// Specifies the running pod (or deployment) to mirror.
///
/// Supports:
/// - `pod/{sample-pod}`;
/// - `podname/{sample-pod}`;
/// - `deployment/{sample-deployment}`;
/// - `container/{sample-container}`;
/// - `containername/{sample-container}`.
///
/// ## Examples
///
/// - Mirror pod `hello-world-abcd-1234`:
///
/// ```toml
/// # mirrord-config.toml
///
/// target = "pod/hello-world-abcd-1234"
/// ```
#[derive(Debug, Deserialize, Clone, Eq, PartialEq, JsonSchema)]
#[serde(untagged)]
pub enum Target {
    /// Mirror a deployment.
    Deployment(DeploymentTarget),
    /// Mirror a pod.
    Pod(PodTarget),
}

impl FromStr for Target {
    type Err = ConfigError;

    fn from_str(target: &str) -> Result<Target> {
        let mut split = target.split('/');
        match split.next() {
            Some("deployment") | Some("deploy") => {
                DeploymentTarget::from_split(&mut split).map(Target::Deployment)
            }
            Some("pod") => PodTarget::from_split(&mut split).map(Target::Pod),
            _ => Err(ConfigError::InvalidTarget(format!(
                "Provided target: {target:?} is neither a pod or a deployment. Did you mean pod/{target:?} or deployment/{target:?}\n{FAIL_PARSE_DEPLOYMENT_OR_POD}",
            ))),
        }
    }
}

/// Mirror the pod specified by [`PodTarget::pod`].
#[derive(Debug, Deserialize, Clone, Eq, PartialEq, JsonSchema)]
pub struct PodTarget {
    /// Pod to mirror.
    pub pod: String,
    pub container: Option<String>,
}

impl FromSplit for PodTarget {
    fn from_split(split: &mut std::str::Split<char>) -> Result<Self> {
        let pod = split
            .next()
            .ok_or_else(|| ConfigError::InvalidTarget(FAIL_PARSE_DEPLOYMENT_OR_POD.to_string()))?;
        match (split.next(), split.next()) {
            (Some("container"), Some(container)) => Ok(Self {
                pod: pod.to_string(),
                container: Some(container.to_string()),
            }),
            (None, None) => Ok(Self {
                pod: pod.to_string(),
                container: None,
            }),
            _ => Err(ConfigError::InvalidTarget(
                FAIL_PARSE_DEPLOYMENT_OR_POD.to_string(),
            )),
        }
    }
}

/// Mirror the deployment specified by [`PodTarget::deployment`].
#[derive(Debug, Deserialize, Clone, Eq, PartialEq, JsonSchema)]
pub struct DeploymentTarget {
    /// Deployment to mirror.
    pub deployment: String,
    pub container: Option<String>,
}

impl FromSplit for DeploymentTarget {
    fn from_split(split: &mut std::str::Split<char>) -> Result<Self> {
        let deployment = split
            .next()
            .ok_or_else(|| ConfigError::InvalidTarget(FAIL_PARSE_DEPLOYMENT_OR_POD.to_string()))?;
        match (split.next(), split.next()) {
            (Some("container"), Some(container)) => Ok(Self {
                deployment: deployment.to_string(),
                container: Some(container.to_string()),
            }),
            (None, None) => Ok(Self {
                deployment: deployment.to_string(),
                container: None,
            }),
            _ => Err(ConfigError::InvalidTarget(
                FAIL_PARSE_DEPLOYMENT_OR_POD.to_string(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::{config::MirrordConfig, util::testing::with_env_vars};

    #[rstest]
    fn default(
        #[values((None, None), (Some("pod/foobar"), Some(Target::Pod(PodTarget { pod: "foobar".to_string(), container: None }))))]
        path: (Option<&str>, Option<Target>),
        #[values((None, None))] namespace: (Option<&str>, Option<&str>),
    ) {
        with_env_vars(
            vec![
                ("MIRRORD_IMPERSONATED_TARGET", path.0),
                ("MIRRORD_TARGET_NAMESPACE", namespace.0),
            ],
            || {
                let target = TargetFileConfig::default().generate_config().unwrap();

                assert_eq!(target.path, path.1);
                assert_eq!(target.namespace.as_deref(), namespace.1);
            },
        );
    }
}
