use core::fmt;
use std::str::FromStr;

use cron_job::CronJobTarget;
use mirrord_analytics::CollectAnalytics;
use schemars::{gen::SchemaGenerator, schema::SchemaObject, JsonSchema};
use serde::{Deserialize, Serialize};
use stateful_set::StatefulSetTarget;

use self::{deployment::DeploymentTarget, job::JobTarget, pod::PodTarget, rollout::RolloutTarget};
use crate::{
    config::{
        from_env::{FromEnv, FromEnvWithError},
        source::MirrordConfigSource,
        ConfigContext, ConfigError, FromMirrordConfig, MirrordConfig, Result,
    },
    util::string_or_struct_option,
};

pub mod cron_job;
pub mod deployment;
pub mod job;
pub mod pod;
pub mod rollout;
pub mod stateful_set;

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
    /// - `job/{sample-job}` (only when [`copy_target`](#feature-copy_target) is enabled).
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
    >> job/<job-name>[/container/container-name]
    >> cronjob/<cronjob-name>[/container/container-name]
    >> statefulset/<statefulset-name>[/container/container-name]

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
/// - `job/{sample-job}`;
/// - `cronjob/{sample-cronjob}`;
/// - `statefulset/{sample-statefulset}`;
#[warn(clippy::wildcard_enum_match_arm)]
#[derive(Serialize, Deserialize, Clone, Eq, PartialEq, Hash, Debug, JsonSchema)]
#[serde(untagged, deny_unknown_fields)]
pub enum Target {
    /// <!--${internal}-->
    /// Mirror a deployment.
    Deployment(deployment::DeploymentTarget),

    /// <!--${internal}-->
    /// Mirror a pod.
    Pod(pod::PodTarget),

    /// <!--${internal}-->
    /// Mirror a rollout.
    Rollout(rollout::RolloutTarget),

    /// <!--${internal}-->
    /// Mirror a Job.
    ///
    /// Only supported when `copy_target` is enabled.
    Job(job::JobTarget),

    /// <!--${internal}-->
    /// Targets a
    /// [CronJob](https://kubernetes.io/docs/concepts/workloads/controllers/cron-jobs/).
    ///
    /// Only supported when `copy_target` is enabled.
    CronJob(cron_job::CronJobTarget),

    /// <!--${internal}-->
    /// Targets a
    /// [StatefulSet](https://kubernetes.io/docs/concepts/workloads/controllers/statefulset/).
    ///
    /// Only supported when `copy_target` is enabled.
    StatefulSet(stateful_set::StatefulSetTarget),

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
                deployment::DeploymentTarget::from_split(&mut split).map(Target::Deployment)
            }
            Some("rollout") => rollout::RolloutTarget::from_split(&mut split).map(Target::Rollout),
            Some("pod") => pod::PodTarget::from_split(&mut split).map(Target::Pod),
            Some("job") => job::JobTarget::from_split(&mut split).map(Target::Job),
            Some("cronjob") => cron_job::CronJobTarget::from_split(&mut split).map(Target::CronJob),
            Some("statefulset") => stateful_set::StatefulSetTarget::from_split(&mut split).map(Target::StatefulSet),
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
            Target::Deployment(target) => target.deployment.clone(),
            Target::Pod(target) => target.pod.clone(),
            Target::Rollout(target) => target.rollout.clone(),
            Target::Job(target) => target.job.clone(),
            Target::CronJob(target) => target.cron_job.clone(),
            Target::StatefulSet(target) => target.stateful_set.clone(),
            Target::Targetless => {
                unreachable!("this shouldn't happen - called from operator on a flow where it's not targetless.")
            }
        }
    }

    /// Get the target type - "pod", "deployment", "rollout" or "targetless"
    pub fn get_target_type(&self) -> &str {
        match self {
            Target::Targetless => "targetless",
            Target::Pod(pod) => pod.type_(),
            Target::Deployment(dep) => dep.type_(),
            Target::Rollout(roll) => roll.type_(),
            Target::Job(job) => job.type_(),
            Target::CronJob(cron_job) => cron_job.type_(),
            Target::StatefulSet(stateful_set) => stateful_set.type_(),
        }
    }

    /// `true` if this [`Target`] is only supported when the copy target feature is enabled.
    pub(super) fn requires_copy(&self) -> bool {
        matches!(
            self,
            Target::Job(_) | Target::CronJob(_) | Target::StatefulSet(_)
        )
    }
}

/// Trait used to convert different aspects of a [`Target`] into a string.
///
/// It's mainly implemented using the `impl_target_display` macro, except for [`Target`]
/// and `TargetHandle`, which manually implement this.
pub trait TargetDisplay {
    /// The string version of a [`Target`]'s type, e.g. `Pod` -> `"Pod"`.
    fn type_(&self) -> &str;

    /// The `name` of a [`Target`], e.g. `"pod-of-beans"`.
    fn name(&self) -> &str;

    /// The optional name of a [`Target`]'s container, e.g. `"can-of-beans"`.
    fn container(&self) -> Option<&String>;
}

/// Implements the [`TargetDisplay`] and [`fmt::Display`] traits for a target type.
macro_rules! impl_target_display {
    ($struct_name:ident, $target_type:ident) => {
        impl TargetDisplay for $struct_name {
            fn type_(&self) -> &str {
                stringify!($target_type)
            }

            fn name(&self) -> &str {
                self.$target_type.as_str()
            }

            fn container(&self) -> Option<&String> {
                self.container.as_ref()
            }
        }

        impl fmt::Display for $struct_name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(
                    f,
                    "{}/{}{}",
                    self.type_(),
                    self.name(),
                    self.container()
                        .map(|name| format!("/container/{name}"))
                        .unwrap_or_default()
                )
            }
        }
    };
}

impl_target_display!(PodTarget, pod);
impl_target_display!(DeploymentTarget, deployment);
impl_target_display!(RolloutTarget, rollout);
impl_target_display!(JobTarget, job);
impl_target_display!(CronJobTarget, cron_job);
impl_target_display!(StatefulSetTarget, stateful_set);

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Target::Targetless => write!(f, "targetless"),
            Target::Pod(target) => target.fmt(f),
            Target::Deployment(target) => target.fmt(f),
            Target::Rollout(target) => target.fmt(f),
            Target::Job(target) => target.fmt(f),
            Target::CronJob(target) => target.fmt(f),
            Target::StatefulSet(target) => target.fmt(f),
        }
    }
}

impl TargetDisplay for Target {
    #[tracing::instrument(level = "trace", ret)]
    fn type_(&self) -> &str {
        match self {
            Target::Targetless => "targetless",
            Target::Deployment(target) => target.type_(),
            Target::Pod(target) => target.type_(),
            Target::Rollout(target) => target.type_(),
            Target::Job(target) => target.type_(),
            Target::CronJob(target) => target.type_(),
            Target::StatefulSet(target) => target.type_(),
        }
    }

    #[tracing::instrument(level = "trace", ret)]
    fn name(&self) -> &str {
        match self {
            Target::Targetless => "targetless",
            Target::Deployment(target) => target.name(),
            Target::Pod(target) => target.name(),
            Target::Rollout(target) => target.name(),
            Target::Job(target) => target.name(),
            Target::CronJob(target) => target.name(),
            Target::StatefulSet(target) => target.name(),
        }
    }

    #[tracing::instrument(level = "trace", ret)]
    fn container(&self) -> Option<&String> {
        match self {
            Target::Targetless => None,
            Target::Deployment(target) => target.container(),
            Target::Pod(target) => target.container(),
            Target::Rollout(target) => target.container(),
            Target::Job(target) => target.container(),
            Target::CronJob(target) => target.container(),
            Target::StatefulSet(target) => target.container(),
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
        const JOB = 32;
        const CRON_JOB = 64;
        const STATEFUL_SET = 128;
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
                Target::Pod(target) => {
                    flags |= TargetAnalyticFlags::POD;
                    if target.container.is_some() {
                        flags |= TargetAnalyticFlags::CONTAINER;
                    }
                }
                Target::Deployment(target) => {
                    flags |= TargetAnalyticFlags::DEPLOYMENT;
                    if target.container.is_some() {
                        flags |= TargetAnalyticFlags::CONTAINER;
                    }
                }
                Target::Rollout(target) => {
                    flags |= TargetAnalyticFlags::ROLLOUT;
                    if target.container.is_some() {
                        flags |= TargetAnalyticFlags::CONTAINER;
                    }
                }
                Target::Job(target) => {
                    flags |= TargetAnalyticFlags::JOB;
                    if target.container.is_some() {
                        flags |= TargetAnalyticFlags::CONTAINER;
                    }
                }
                Target::CronJob(target) => {
                    flags |= TargetAnalyticFlags::CRON_JOB;
                    if target.container.is_some() {
                        flags |= TargetAnalyticFlags::CONTAINER;
                    }
                }
                Target::StatefulSet(target) => {
                    flags |= TargetAnalyticFlags::STATEFUL_SET;
                    if target.container.is_some() {
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
