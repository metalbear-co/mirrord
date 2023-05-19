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

#[derive(Serialize, Deserialize, Clone, Eq, PartialEq, Hash, Debug)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TargetConfig {
    pub path: Target,
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
    fn get_optional_path(path_from_config_file: Option<Target>) -> Result<Option<Target>> {
        FromEnvWithError::new("MIRRORD_IMPERSONATED_TARGET")
            .or(path_from_config_file)
            .source_value()
            .transpose()
    }
}

impl MirrordConfig for TargetFileConfig {
    type Generated = Option<TargetConfig>;

    /// Generate the final config object, out of the configuration parsed from a configuration file,
    /// factoring in environment variables (which are also set by the front end - CLI/IDE-plugin).
    ///
    /// `None` if no target specified.
    /// Specifying target namespace without target is not allowed and results in an error that
    /// explains to the user what to do instead.
    fn generate_config(self) -> Result<Self::Generated> {
        let config = match self {
            TargetFileConfig::Simple(path) => {
                // Namespace was not specified via file, get it from env var if set.
                let namespace: Option<String> = FromEnv::new("MIRRORD_TARGET_NAMESPACE")
                    .source_value()
                    .transpose()?;

                let path = if let Some(path) = Self::get_optional_path(path)? {
                    path
                } else {
                    // No path specified, neither via file, nor via env. So no `TargetConfig`,
                    // running targetless.

                    if let Some(namespace_from_env) = namespace {
                        // env var (or CLI flag) was set.

                        if !namespace_from_env.is_empty() {
                            // And the value is not the empty string.

                            return Err(ConfigError::TargetNamespaceWithoutTarget);
                        }
                    }
                    return Ok(None); // Run targetless.
                };
                TargetConfig { path, namespace }
            }
            TargetFileConfig::Advanced { path, namespace } => {
                // this assertion panics when running in ext/ls mode
                // debug_assert!(namespace.is_some()); // Should only be advanced if namespace
                // there.
                let path = if let Some(path) = Self::get_optional_path(path)? {
                    path
                } else {
                    return Err(ConfigError::TargetNamespaceWithoutTarget);
                };
                TargetConfig {
                    path,
                    namespace: FromEnv::new("MIRRORD_TARGET_NAMESPACE")
                        .or(namespace)
                        .source_value()
                        .transpose()?,
                }
            }
        };

        Ok(Some(config))
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
    #[case(None, None, None)] // Nothing specified - no target config (targetless mode).
    #[should_panic]
    #[case(None, Some("ns"), None)] // Namespace without target - error.
    #[case(
        Some("pod/foo"),
        None,
        Some(TargetConfig{
            path: Target::Pod(PodTarget {pod: "foo".to_string(), container: None}),
            namespace: None
        })
    )] // Only pod specified
    #[case(
        Some("pod/foo/container/bar"),
        None,
        Some(TargetConfig{
            path: Target::Pod(PodTarget {
                pod: "foo".to_string(),
                container: Some("bar".to_string())
            }),
            namespace: None
        })
    )] // Pod and container specified.
    #[case(
        Some("pod/foo"),
        Some("baz"),
        Some(TargetConfig{
            path: Target::Pod(PodTarget {pod: "foo".to_string(), container: None}),
            namespace: Some("baz".to_string())
        })
    )] // Pod and namespace specified.
    fn default(
        #[case] path_env: Option<&str>,
        #[case] namespace_env: Option<&str>,
        #[case] expected_target_config: Option<TargetConfig>,
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
