use std::{
    fmt::{self, Display},
    str::FromStr,
};

use mirrord_analytics::CollectAnalytics;
use schemars::{gen::SchemaGenerator, schema::SchemaObject, JsonSchema};
use serde::{Deserialize, Serialize};

use crate::{
    config::{
        from_env::{FromEnv, FromEnvWithError},
        source::MirrordConfigSource,
        ConfigContext, ConfigError, FromMirrordConfig, MirrordConfig, Result,
    },
    util::string_or_struct_option,
};

#[derive(Deserialize, PartialEq, Eq, Clone, Debug, JsonSchema)]
#[serde(untagged, rename_all = "lowercase", deny_unknown_fields)]
pub enum TargetFileConfig {
    // Generated when the value of the `target` field is a string, or when there is no target.
    // we need default else target value will be required in some scenarios.
    Simple(
        #[serde(default, deserialize_with = "string_or_struct_option")]
        #[schemars(schema_with = "make_simple_target_custom_schema")]
        Option<Target>,
    ),
    Advanced {
        /// <!--${internal}-->
        /// Path is optional so that it can also be specified via env var instead of via conf file,
        /// but it is not optional in a resulting [`TargetConfig`] object - either there is a path,
        /// or the target configuration is `None`.
        #[serde(default, deserialize_with = "string_or_struct_option")]
        path: Option<Target>,
        namespace: Option<String>,
    },
}

fn make_simple_target_custom_schema(gen: &mut SchemaGenerator) -> schemars::schema::Schema {
    // generate the schema for the Option<Target> like usual, then just push a string type to the
    // any_of.
    let mut schema: SchemaObject = <Option<Target>>::json_schema(gen).into();
    let subschema = schema.subschemas();

    let mut any_ofs = subschema.any_of.clone().unwrap();
    any_ofs.push(
        // There's a small gap here for the string to be _anything_, not just k8s objects.
        schemars::schema::SchemaObject {
            instance_type: Some(schemars::schema::InstanceType::String.into()),
            ..Default::default()
        }
        .into(),
    );
    subschema.any_of = Some(any_ofs);

    schema.into()
}

// - Only path is `Some` -> use current namespace.
// - Only namespace is `Some` -> this should only happen in `mirrord ls`. In `mirrord exec`
//   namespace without a path does not mean anything and therefore should be prevented by returning
//   an error. The error is not returned when parsing the configuration because it's not an error
//   for `mirrord ls`.
// - Both are `None` -> targetless.
/// Specifies the target and namespace to mirror, see [`path`](#target-path) for a list of
/// accepted values for the `target` option.
///
/// The simplified configuration supports:
///
/// - `pod/{sample-pod}/[container]/{sample-container}`;
/// - `podname/{sample-pod}/[container]/{sample-container}`;
/// - `deployment/{sample-deployment}/[container]/{sample-container}`;
///
/// Shortened setup:
///
///```json
/// {
///  "target": "pod/bear-pod"
/// }
/// ```
///
/// Complete setup:
///
/// ```json
/// {
///  "target": {
///    "path": {
///      "pod": "bear-pod"
///    },
///    "namespace": "default"
///  }
/// }
/// ```
#[derive(Serialize, Deserialize, Clone, Eq, PartialEq, Hash, Debug)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct TargetConfig {
    /// ### target.path {#target-path}
    ///
    /// Specifies the running pod (or deployment) to mirror.
    ///
    /// Note: Deployment level steal/mirroring is available only in mirrord for Teams
    /// If you use it without it, it will choose a random pod replica to work with.
    ///
    /// Supports:
    /// - `pod/{sample-pod}`;
    /// - `podname/{sample-pod}`;
    /// - `deployment/{sample-deployment}`;
    /// - `container/{sample-container}`;
    /// - `containername/{sample-container}`.
    pub path: Option<Target>,

    /// ### target.namespace {#target-namespace}
    ///
    /// Namespace where the target lives.
    ///
    /// Defaults to `"default"`.
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
    fn get_target_path_from_env(context: &mut ConfigContext) -> Result<Option<Target>> {
        FromEnvWithError::new("MIRRORD_IMPERSONATED_TARGET")
            .source_value(context)
            .transpose()
    }

    /// Get the target namespace from the env var, `Ok(None)` if not set, `Err` if invalid value.
    fn get_target_namespace_from_env(context: &mut ConfigContext) -> Result<Option<String>> {
        FromEnv::new("MIRRORD_TARGET_NAMESPACE")
            .source_value(context)
            .transpose()
    }
}

impl MirrordConfig for TargetFileConfig {
    type Generated = TargetConfig;

    /// Generate the final config object, out of the configuration parsed from a configuration file,
    /// factoring in environment variables (which are also set by the front end - CLI/IDE-plugin).
    fn generate_config(self, context: &mut ConfigContext) -> Result<Self::Generated> {
        let (path_from_conf_file, namespace_from_conf_file) = match self {
            TargetFileConfig::Simple(path) => (path, None),
            TargetFileConfig::Advanced { path, namespace } => (path, namespace),
        };

        // Env overrides configuration if both there.
        let path = Self::get_target_path_from_env(context)?.or(path_from_conf_file);
        let namespace = Self::get_target_namespace_from_env(context)?.or(namespace_from_conf_file);
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

/// <!--${internal}-->
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
#[derive(Serialize, Deserialize, Clone, Eq, PartialEq, Hash, Debug, JsonSchema)]
#[serde(untagged, deny_unknown_fields)]
pub enum Target {
    /// <!--${internal}-->
    /// Mirror a deployment.
    Deployment(DeploymentTarget),

    /// <!--${internal}-->
    /// Mirror a pod.
    Pod(PodTarget),

    /// <!--${internal}-->
    /// Mirror a rollout.
    Rollout(RolloutTarget),

    /// <!--${internal}-->
    /// Spawn a new pod.
    Targetless,
}

impl FromStr for Target {
    type Err = ConfigError;

    fn from_str(target: &str) -> Result<Target> {
        if target == "targetless" {
            return Ok(Target::Targetless);
        }
        let mut split = target.split('/');
        match split.next() {
            Some("deployment") | Some("deploy") => {
                DeploymentTarget::from_split(&mut split).map(Target::Deployment)
            }
            Some("rollout") => RolloutTarget::from_split(&mut split).map(Target::Rollout),
            Some("pod") => PodTarget::from_split(&mut split).map(Target::Pod),
            _ => Err(ConfigError::InvalidTarget(format!(
                "Provided target: {target} is unsupported. Did you remember to add a prefix, e.g. pod/{target}? \n{FAIL_PARSE_DEPLOYMENT_OR_POD}",
            ))),
        }
    }
}

impl Target {
    /// Get the target name - pod name, deployment name, rollout name..
    pub fn get_target_name(&self) -> String {
        match self {
            Target::Deployment(deployment) => deployment.deployment.clone(),
            Target::Pod(pod) => pod.pod.clone(),
            Target::Rollout(rollout) => rollout.rollout.clone(),
            Target::Targetless => {
                unreachable!("this shouldn't happen - called from operator on a flow where it's not targetless.")
            }
        }
    }
}

trait TargetDisplay {
    fn target_type(&self) -> &str;

    fn target_name(&self) -> &str;

    fn container_name(&self) -> Option<&String>;

    fn fmt_display(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}/{}{}",
            self.target_type(),
            self.target_name(),
            self.container_name()
                .map(|name| format!("/container/{name}"))
                .unwrap_or_default()
        )
    }
}

macro_rules! impl_target_display {
    ($struct_name:ident, $target_type:ident) => {
        impl TargetDisplay for $struct_name {
            fn target_type(&self) -> &str {
                stringify!($target_type)
            }

            fn target_name(&self) -> &str {
                self.$target_type.as_str()
            }

            fn container_name(&self) -> Option<&String> {
                self.container.as_ref()
            }
        }
    };
}

impl_target_display!(PodTarget, pod);
impl_target_display!(DeploymentTarget, deployment);
impl_target_display!(RolloutTarget, rollout);

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Target::Targetless => write!(f, "targetless"),
            Target::Pod(pod) => pod.fmt_display(f),
            Target::Deployment(dep) => dep.fmt_display(f),
            Target::Rollout(roll) => roll.fmt_display(f),
        }
    }
}

/// <!--${internal}-->
/// Mirror the pod specified by [`PodTarget::pod`].
#[derive(Serialize, Deserialize, Clone, Eq, PartialEq, Hash, Debug, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PodTarget {
    /// <!--${internal}-->
    /// Pod to mirror.
    pub pod: String,
    pub container: Option<String>,
}

impl Display for PodTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}{}",
            self.container
                .as_ref()
                .map(|c| format!("{c}/"))
                .unwrap_or_default(),
            self.pod.clone()
        )
    }
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

/// <!--${internal}-->
/// Mirror the rollout specified by [`RolloutTarget::rollout`].
#[derive(Serialize, Deserialize, Clone, Eq, PartialEq, Hash, Debug, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RolloutTarget {
    /// <!--${internal}-->
    /// Rollout to mirror.
    pub rollout: String,
    pub container: Option<String>,
}

impl FromSplit for RolloutTarget {
    fn from_split(split: &mut std::str::Split<char>) -> Result<Self> {
        let rollout = split
            .next()
            .ok_or_else(|| ConfigError::InvalidTarget(FAIL_PARSE_DEPLOYMENT_OR_POD.to_string()))?;
        match (split.next(), split.next()) {
            (Some("container"), Some(container)) => Ok(Self {
                rollout: rollout.to_string(),
                container: Some(container.to_string()),
            }),
            (None, None) => Ok(Self {
                rollout: rollout.to_string(),
                container: None,
            }),
            _ => Err(ConfigError::InvalidTarget(
                FAIL_PARSE_DEPLOYMENT_OR_POD.to_string(),
            )),
        }
    }
}

bitflags::bitflags! {
    #[repr(C)]
    #[derive(Debug, PartialEq, Eq)]
    pub struct TargetAnalyticFlags: u32 {
        const NAMESPACE = 1;
        const POD = 2;
        const DEPLOYMENT = 4;
        const CONTAINER = 8;
        const ROLLOUT = 16;
    }
}

impl CollectAnalytics for &TargetConfig {
    fn collect_analytics(&self, analytics: &mut mirrord_analytics::Analytics) {
        let mut flags = TargetAnalyticFlags::empty();
        if self.namespace.is_some() {
            flags |= TargetAnalyticFlags::NAMESPACE;
        }
        if let Some(path) = &self.path {
            match path {
                Target::Pod(pod) => {
                    flags |= TargetAnalyticFlags::POD;
                    if pod.container.is_some() {
                        flags |= TargetAnalyticFlags::CONTAINER;
                    }
                }
                Target::Deployment(deployment) => {
                    flags |= TargetAnalyticFlags::DEPLOYMENT;
                    if deployment.container.is_some() {
                        flags |= TargetAnalyticFlags::CONTAINER;
                    }
                }
                Target::Rollout(rollout) => {
                    flags |= TargetAnalyticFlags::ROLLOUT;
                    if rollout.container.is_some() {
                        flags |= TargetAnalyticFlags::CONTAINER;
                    }
                }
                Target::Targetless => {
                    // Targetless is essentially 0, so no need to set any flags.
                }
            }
        }
        analytics.add("target_mode", flags.bits())
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::{
        config::{ConfigContext, MirrordConfig},
        util::testing::with_env_vars,
    };

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
    #[case(
        Some("rollout/foo"),
        None,
        TargetConfig{
            path: Some(Target::Rollout(RolloutTarget {
                rollout: "foo".to_string(),
                container: None
            })),
            namespace: None
        }
    )] // Rollout specified.
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
                let mut cfg_context = ConfigContext::default();
                let generated_target_config = TargetFileConfig::default()
                    .generate_config(&mut cfg_context)
                    .unwrap();

                assert_eq!(expected_target_config, generated_target_config);
            },
        );
    }

    /// Parse a json string, generate a `TargetConfig` out of it, and compare to expected config.
    fn verify_config(config_json_string: &str, expected_target_config: &TargetConfig) {
        let target_file_config: TargetFileConfig =
            serde_json::from_str(config_json_string).unwrap();
        let mut cfg_context = ConfigContext::default();
        let target_config: TargetConfig = target_file_config
            .generate_config(&mut cfg_context)
            .unwrap();
        assert_eq!(&target_config, expected_target_config);
    }

    /// Test parsing the configuration when the env vars are not set.
    #[rstest]
    // advanced variant, namespace only.
    #[case(
        r#"{ "namespace": "my-test-namespace" }"#,
        TargetConfig {
            path: None,
            namespace: Some("my-test-namespace".to_string())
        }
    )]
    // simple variant of file config - path string, not an object.
    #[case(
        r#""pod/my-cool-pod""#,
        TargetConfig{
            path: Some(Target::Pod(PodTarget {pod: "my-cool-pod".to_string(), container: None})),
            namespace: None
        }
    )]
    // advanced variant of file config.
    #[case(
        r#"{ "path": "pod/my-cool-pod" }"#,
        TargetConfig{
            path: Some(Target::Pod(PodTarget {pod: "my-cool-pod".to_string(), container: None})),
            namespace: None
        }
    )]
    // advanced variant of file config, with object as path.
    #[case(
        r#"{
            "path": {
                "pod": "my-cool-pod"
            }
        }"#,
        TargetConfig{
            path: Some(Target::Pod(PodTarget {pod: "my-cool-pod".to_string(), container: None})),
            namespace: None
        }
    )]
    fn parse_target_config_from_json(
        #[case] config_json_string: &str,
        #[case] mut expected_target_config: TargetConfig,
    ) {
        // First test without env vars.
        with_env_vars(
            vec![
                ("MIRRORD_IMPERSONATED_TARGET", None),
                ("MIRRORD_TARGET_NAMESPACE", None),
            ],
            || verify_config(config_json_string, &expected_target_config),
        );

        // Now test that the namespace is set (overridden) by the env var.
        let namespace = "override-namespace";
        expected_target_config.namespace = Some(namespace.to_string());
        with_env_vars(
            vec![
                ("MIRRORD_IMPERSONATED_TARGET", None),
                ("MIRRORD_TARGET_NAMESPACE", Some(namespace)),
            ],
            || verify_config(config_json_string, &expected_target_config),
        );
    }
}
