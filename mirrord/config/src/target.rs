use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    config::{
        from_env::{FromEnv, FromEnvWithError},
        source::MirrordConfigSource,
        ConfigError, FromMirrordConfig, MirrordConfig, Result,
    },
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
    // Generated when the value of the `target` field is a string, or when there is no target.
    // we need default else target value will be required in some scenarios.
    Simple(#[serde(default, deserialize_with = "string_or_struct_option")] Option<Target>),
    Advanced {
        // Path is optional so that it can also be specified via env var instead of via conf file,
        // but it is not optional in a resulting [`TargetConfig`] object - either there is a path,
        // or the target configuration is `None`.
        #[serde(default, deserialize_with = "string_or_struct_option")]
        path: Option<Target>,
        namespace: Option<String>,
    },
}

// - Only path is `Some` -> use current namespace.
// - Only namespace is `Some` -> this should only happen in `mirrord ls`. In `mirrord exec`
//   namespace without a path does not mean anything and therefore should be prevented by returning
//   an error. The error is not returned when parsing the configuration because it's not an error
//   for `mirrord ls`.
// - Both are `None` -> targetless.
#[derive(Serialize, Deserialize, Clone, Eq, PartialEq, Hash, Debug)]
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

impl FromMirrordConfig for TargetConfig {
    type Generator = TargetFileConfig;
}

impl TargetFileConfig {
    /// Get the target path from the env var, `Ok(None)` if not set, `Err` if invalid value.
    fn get_target_path_from_env() -> Result<Option<Target>> {
        FromEnvWithError::new("MIRRORD_IMPERSONATED_TARGET")
            .source_value()
            .transpose()
    }

    /// Get the target namespace from the env var, `Ok(None)` if not set, `Err` if invalid value.
    fn get_target_namespace_from_env() -> Result<Option<String>> {
        FromEnv::new("MIRRORD_TARGET_NAMESPACE")
            .source_value()
            .transpose()
    }
}

impl MirrordConfig for TargetFileConfig {
    type Generated = TargetConfig;

    /// Generate the final config object, out of the configuration parsed from a configuration file,
    /// factoring in environment variables (which are also set by the front end - CLI/IDE-plugin).
    fn generate_config(self) -> Result<Self::Generated> {
        let (path_from_conf_file, namespace_from_conf_file) = match self {
            TargetFileConfig::Simple(path) => (path, None),
            TargetFileConfig::Advanced { path, namespace } => (path, namespace),
        };

        // Env overrides configuration if both there.
        let path = Self::get_target_path_from_env()?.or(path_from_conf_file);
        let namespace = Self::get_target_namespace_from_env()?.or(namespace_from_conf_file);
        Ok(TargetConfig { path, namespace })
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
#[derive(Serialize, Deserialize, Clone, Eq, PartialEq, Hash, Debug, JsonSchema)]
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
                "Provided target: {target} is neither a pod or a deployment. Did you mean pod/{target} or deployment/{target}\n{FAIL_PARSE_DEPLOYMENT_OR_POD}",
            ))),
        }
    }
}

/// Mirror the pod specified by [`PodTarget::pod`].
#[derive(Serialize, Deserialize, Clone, Eq, PartialEq, Hash, Debug, JsonSchema)]
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

/// Mirror the deployment specified by [`DeploymentTarget::deployment`].
#[derive(Serialize, Deserialize, Clone, Eq, PartialEq, Hash, Debug, JsonSchema)]
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
    #[case(None, None,
        TargetConfig {
            path: None,
            namespace: None
        }
    )] // Nothing specified - no target config (targetless mode).
    #[case(
        None,
        Some("ns"),
        TargetConfig{
            path: None,
            namespace: Some("ns".to_string())
        }
    )] // Namespace without target - error.
    #[case(
        Some("pod/foo"),
        None,
        TargetConfig{
            path: Some(Target::Pod(PodTarget {pod: "foo".to_string(), container: None})),
            namespace: None
        }
    )] // Only pod specified
    #[case(
        Some("pod/foo/container/bar"),
        None,
        TargetConfig{
            path: Some(Target::Pod(PodTarget {
                pod: "foo".to_string(),
                container: Some("bar".to_string())
            })),
            namespace: None
        }
    )] // Pod and container specified.
    #[case(
        Some("pod/foo"),
        Some("baz"),
        TargetConfig{
            path: Some(Target::Pod(PodTarget {pod: "foo".to_string(), container: None})),
            namespace: Some("baz".to_string())
        }
    )] // Pod and namespace specified.
    fn default(
        #[case] path_env: Option<&str>,
        #[case] namespace_env: Option<&str>,
        #[case] expected_target_config: TargetConfig,
    ) {
        with_env_vars(
            vec![
                ("MIRRORD_IMPERSONATED_TARGET", path_env),
                ("MIRRORD_TARGET_NAMESPACE", namespace_env),
            ],
            || {
                let generated_target_config =
                    TargetFileConfig::default().generate_config().unwrap();

                assert_eq!(expected_target_config, generated_target_config);
            },
        );
    }
}
