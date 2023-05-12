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

/// ## target {#target}
///
/// Specifies the target and namespace to mirror, see [`path`](##path) for a list of accepted values
/// for the `target` option.
///
/// ### Minimal `target` config {#target-minimal}
///
/// ```json
/// {
///   "target": "pod/bear-pod"
/// }
/// ```
///
/// ### Advanced `target` config {#target-advanced}
///
/// ```json
/// {
///   "target": {
///     "path": {
///       "pod": "bear-pod"
///     },
///     "namespace": "default"
///   }
/// }
/// ```
///
/// ### target.path {#target-path}
///
/// Specifies the running pod (or deployment) to mirror.
///
/// Supports:
/// - `pod/{sample-pod}`;
/// - `podname/{sample-pod}`;
/// - `deployment/{sample-deployment}`;
/// - `container/{sample-container}`;
/// - `containername/{sample-container}`.
///
/// ### target.namespace {#target-namespace}
///
/// The namespace of the remote pod.
///
/// Defaults to `"default"`.
#[derive(Deserialize, PartialEq, Eq, Clone, Debug, JsonSchema)]
#[serde(untagged, rename_all = "lowercase")]
pub enum TargetFileConfig {
    // we need default else target value will be required in some scenarios.
    Simple(#[serde(default, deserialize_with = "string_or_struct_option")] Option<Target>),
    Advanced {
        #[serde(default, deserialize_with = "string_or_struct_option")]
        path: Option<Target>,
        namespace: Option<String>,
    },
}

#[derive(Serialize, Deserialize, Clone, Eq, PartialEq, Hash, Debug)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TargetConfig {
    // rustdoc-stripper-ignore-next
    /// ### target.path
    ///
    /// Path of the target to impersonate.
    pub path: Option<Target>,

    /// ### target.namespace
    ///
    /// Namespace where the target lives.
    ///
    /// Defaults to `"default"`.
    // rustdoc-stripper-ignore-next-stop
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

impl MirrordConfig for TargetFileConfig {
    type Generated = TargetConfig;

    fn generate_config(self) -> Result<Self::Generated> {
        let config = match self {
            TargetFileConfig::Simple(path) => TargetConfig {
                path: FromEnvWithError::new("MIRRORD_IMPERSONATED_TARGET")
                    .or(path)
                    .source_value()
                    .transpose()?,
                namespace: FromEnv::new("MIRRORD_TARGET_NAMESPACE")
                    .source_value()
                    .transpose()?,
            },
            TargetFileConfig::Advanced { path, namespace } => TargetConfig {
                path: FromEnvWithError::new("MIRRORD_IMPERSONATED_TARGET")
                    .or(path)
                    .source_value()
                    .transpose()?,
                namespace: FromEnv::new("MIRRORD_TARGET_NAMESPACE")
                    .or(namespace)
                    .source_value()
                    .transpose()?,
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

// rustdoc-stripper-ignore-next
/// ## path
///
/// Specifies the running pod (or deployment) to mirror.
///
/// Supports:
/// - `pod/{sample-pod}`;
/// - `podname/{sample-pod}`;
/// - `deployment/{sample-deployment}`;
/// - `container/{sample-container}`;
/// - `containername/{sample-container}`.
// rustdoc-stripper-ignore-next-stop
#[derive(Serialize, Deserialize, Clone, Eq, PartialEq, Hash, Debug, JsonSchema)]
#[serde(untagged)]
pub enum Target {
    // rustdoc-stripper-ignore-next
    /// Mirror a deployment.
    // rustdoc-stripper-ignore-next-stop
    Deployment(DeploymentTarget),

    // rustdoc-stripper-ignore-next
    /// Mirror a pod.
    // rustdoc-stripper-ignore-next-stop
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

// rustdoc-stripper-ignore-next
/// Mirror the pod specified by [`PodTarget::pod`].
// rustdoc-stripper-ignore-next-stop
#[derive(Serialize, Deserialize, Clone, Eq, PartialEq, Hash, Debug, JsonSchema)]
pub struct PodTarget {
    // rustdoc-stripper-ignore-next
    /// Pod to mirror.
    // rustdoc-stripper-ignore-next-stop
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

// rustdoc-stripper-ignore-next
/// Mirror the deployment specified by [`DeploymentTarget::deployment`].
// rustdoc-stripper-ignore-next-stop
#[derive(Serialize, Deserialize, Clone, Eq, PartialEq, Hash, Debug, JsonSchema)]
pub struct DeploymentTarget {
    // rustdoc-stripper-ignore-next
    /// Deployment to mirror.
    // rustdoc-stripper-ignore-next-stop
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
        #[values((None, None), (Some("foo"), Some("foo")))] namespace: (Option<&str>, Option<&str>),
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
