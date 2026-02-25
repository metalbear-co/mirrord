use std::{fmt, str::FromStr};

use cron_job::CronJobTarget;
use mirrord_analytics::CollectAnalytics;
use replica_set::ReplicaSetTarget;
use schemars::{JsonSchema, r#gen::SchemaGenerator, schema::SchemaObject};
use serde::{Deserialize, Serialize};
use strum_macros::{EnumDiscriminants, EnumString};

use self::{
    deployment::DeploymentTarget, job::JobTarget, pod::PodTarget, rollout::RolloutTarget,
    service::ServiceTarget, stateful_set::StatefulSetTarget,
};
use crate::{
    config::{
        ConfigContext, ConfigError, FromMirrordConfig, MirrordConfig, Result,
        from_env::{FromEnv, FromEnvWithError},
        source::MirrordConfigSource,
    },
    feature::FeatureConfig,
    util::string_or_struct_option,
};

pub mod cron_job;
pub mod deployment;
pub mod job;
pub mod pod;
pub mod replica_set;
pub mod rollout;
pub mod service;
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
        #[schemars(schema_with = "make_simple_target_custom_schema")]
        path: Option<Target>,
        namespace: Option<String>,
    },
}

fn make_simple_target_custom_schema(r#gen: &mut SchemaGenerator) -> schemars::schema::Schema {
    // generate the schema for the Option<Target> like usual, then just push a string type to the
    // any_of.
    let mut schema: SchemaObject = <Option<Target>>::json_schema(r#gen).into();
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

/// Specifies the target and namespace to target.
///
/// The simplified configuration supports:
///
/// - `targetless`
/// - `pod/{pod-name}[/container/{container-name}]`;
/// - `deployment/{deployment-name}[/container/{container-name}]`;
/// - `rollout/{rollout-name}[/container/{container-name}]`;
/// - `job/{job-name}[/container/{container-name}]`;
/// - `cronjob/{cronjob-name}[/container/{container-name}]`;
/// - `statefulset/{statefulset-name}[/container/{container-name}]`;
/// - `service/{service-name}[/container/{container-name}]`;
///
/// Please note that:
///
/// - `job`, `cronjob`, `statefulset` and `service` targets require the mirrord Operator
/// - `job` and `cronjob` targets require the [`copy_target`](#feature-copy_target) feature
///
/// Shortened setup with a target:
///
///```json
/// {
///  "target": "pod/bear-pod"
/// }
/// ```
///
/// The setup above will result in a session targeting the `bear-pod` Kubernetes pod
/// in the user's default namespace. A target container will be chosen by mirrord.
///
/// Shortened setup with a target container:
///
/// ```json
/// {
///   "target": "pod/bear-pod/container/bear-pod-container"
/// }
/// ```
///
/// The setup above will result in a session targeting the `bear-pod-container` container
/// in the `bear-pod` Kubernetes pod in the user's default namespace.
///
/// Complete setup with a target container:
///
/// ```json
/// {
///  "target": {
///    "path": {
///      "pod": "bear-pod",
///      "container": "bear-pod-container"
///    },
///    "namespace": "bear-pod-namespace"
///  }
/// }
/// ```
///
/// The setup above will result in a session targeting the `bear-pod-container` container
/// in the `bear-pod` Kubernetes pod in the `bear-pod-namespace` namespace.
///
/// Setup with a namespace for a targetless run:
///
/// ```json
/// {
///   "target": {
///     "path": "targetless",
///     "namespace": "bear-namespace"
///   }
/// }
/// ```
///
/// The setup above will result in a session without any target.
/// Remote outgoing traffic and DNS will be done from the `bear-namespace` namespace.
#[derive(Serialize, Deserialize, Clone, Eq, PartialEq, Hash, Debug)]
#[serde(deny_unknown_fields)]
pub struct TargetConfig {
    /// ### target.path {#target-path}
    ///
    /// Specifies the Kubernetes resource to target.
    ///
    /// If not given, defaults to `targetless`.
    ///
    /// Note: targeting services and whole workloads is available only in mirrord for Teams.
    /// If you target a workload without the mirrord Operator, it will choose a random pod replica
    /// to work with.
    ///
    /// Supports:
    /// - `targetless`
    /// - `pod/{pod-name}[/container/{container-name}]`;
    /// - `deployment/{deployment-name}[/container/{container-name}]`;
    /// - `rollout/{rollout-name}[/container/{container-name}]`;
    /// - `job/{job-name}[/container/{container-name}]`; (requires mirrord Operator and the
    ///   [`copy_target`](#feature-copy_target) feature)
    /// - `cronjob/{cronjob-name}[/container/{container-name}]`; (requires mirrord Operator and the
    ///   [`copy_target`](#feature-copy_target) feature)
    /// - `statefulset/{statefulset-name}[/container/{container-name}]`; (requires mirrord
    ///   Operator)
    /// - `service/{service-name}[/container/{container-name}]`; (requires mirrord Operator)
    /// - `replicaset/{replicaset-name}[/container/{container-name}]`; (requires mirrord Operator)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<Target>,

    /// ### target.namespace {#target-namespace}
    ///
    /// Namespace where the target lives.
    ///
    /// For targetless runs, this the namespace in which remote networking is done.
    ///
    /// Defaults to the Kubernetes user's default namespace (defined in Kubernetes context).
    #[serde(skip_serializing_if = "Option::is_none")]
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
    >> `targetless`
    >> `pod/{pod-name}[/container/{container-name}]`;
    >> `deployment/{deployment-name}[/container/{container-name}]`;
    >> `rollout/{rollout-name}[/container/{container-name}]`;
    >> `job/{job-name}[/container/{container-name}]`;
    >> `cronjob/{cronjob-name}[/container/{container-name}]`;
    >> `statefulset/{statefulset-name}[/container/{container-name}]`;
    >> `service/{service-name}[/container/{container-name}]`;
    >> `replicaset/{replicaset-name}[/container/{container-name}]`;

- Note:
    >> specifying container name is optional, defaults to a container chosen by mirrord
    >> targeting a workload without the mirrord Operator results in a session targeting a random pod replica

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
/// - `targetless`
/// - `pod/{pod-name}[/container/{container-name}]`;
/// - `deployment/{deployment-name}[/container/{container-name}]`;
/// - `rollout/{rollout-name}[/container/{container-name}]`;
/// - `job/{job-name}[/container/{container-name}]`;
/// - `cronjob/{cronjob-name}[/container/{container-name}]`;
/// - `statefulset/{statefulset-name}[/container/{container-name}]`;
/// - `service/{service-name}[/container/{container-name}]`;
/// - `replicaset/{replicaset-name}[/container/{container-name}]`;
///
/// Used to derive `TargetType` via the strum crate
#[warn(clippy::wildcard_enum_match_arm)]
#[derive(Serialize, Deserialize, Clone, Eq, PartialEq, Hash, Debug, EnumDiscriminants)]
#[serde(untagged, deny_unknown_fields)]
#[strum_discriminants(derive(EnumString, Serialize, Deserialize))]
#[strum_discriminants(name(TargetType))]
#[strum_discriminants(strum(serialize_all = "lowercase"))]
#[strum_discriminants(serde(rename_all = "lowercase"))]
pub enum Target {
    /// <!--${internal}-->
    /// [Deployment](https://kubernetes.io/docs/concepts/workloads/controllers/deployment/).
    Deployment(deployment::DeploymentTarget),

    /// <!--${internal}-->
    /// [Pod](https://kubernetes.io/docs/concepts/workloads/pods/).
    Pod(pod::PodTarget),

    /// <!--${internal}-->
    /// [Argo Rollout](https://argoproj.github.io/argo-rollouts/#how-does-it-work).
    Rollout(rollout::RolloutTarget),

    /// <!--${internal}-->
    /// [Job](https://kubernetes.io/docs/concepts/workloads/controllers/job/).
    ///
    /// Only supported when `copy_target` is enabled.
    Job(job::JobTarget),

    /// <!--${internal}-->
    /// [CronJob](https://kubernetes.io/docs/concepts/workloads/controllers/cron-jobs/).
    ///
    /// Only supported when `copy_target` is enabled.
    CronJob(cron_job::CronJobTarget),

    /// <!--${internal}-->
    /// [StatefulSet](https://kubernetes.io/docs/concepts/workloads/controllers/statefulset/).
    StatefulSet(stateful_set::StatefulSetTarget),

    /// <!--${internal}-->
    /// [Service](https://kubernetes.io/docs/concepts/services-networking/service/).
    Service(service::ServiceTarget),

    /// <!--${internal}-->
    /// [ReplicaSet](https://kubernetes.io/docs/concepts/workloads/controllers/replicaset/).
    ReplicaSet(replica_set::ReplicaSetTarget),

    /// <!--${internal}-->
    /// Spawn a new pod.
    Targetless,
}

impl JsonSchema for Target {
    fn schema_name() -> String {
        "Target".to_owned()
    }

    fn json_schema(schema_gen: &mut schemars::r#gen::SchemaGenerator) -> schemars::schema::Schema {
        let mut schema = schemars::schema::SchemaObject::default();

        schema.subschemas().one_of = Some(vec![
            schema_gen.subschema_for::<deployment::DeploymentTarget>(),
            schema_gen.subschema_for::<pod::PodTarget>(),
            schema_gen.subschema_for::<rollout::RolloutTarget>(),
            schema_gen.subschema_for::<job::JobTarget>(),
            schema_gen.subschema_for::<cron_job::CronJobTarget>(),
            schema_gen.subschema_for::<stateful_set::StatefulSetTarget>(),
            schema_gen.subschema_for::<service::ServiceTarget>(),
            schema_gen.subschema_for::<replica_set::ReplicaSetTarget>(),
            schemars::schema::Schema::Object(schemars::schema::SchemaObject {
                enum_values: Some(vec![serde_json::Value::String("targetless".to_string())]),
                ..Default::default()
            }),
        ]);

        schema.into()
    }
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
            Some("statefulset") => {
                stateful_set::StatefulSetTarget::from_split(&mut split).map(Target::StatefulSet)
            }
            Some("service") => service::ServiceTarget::from_split(&mut split).map(Target::Service),
            Some("replicaset") => {
                replica_set::ReplicaSetTarget::from_split(&mut split).map(Target::ReplicaSet)
            }
            _ => Err(ConfigError::InvalidTarget(format!(
                "Provided target: {target} is unsupported. Did you remember to add a prefix, e.g. pod/{target}? \n{FAIL_PARSE_DEPLOYMENT_OR_POD}",
            ))),
        }
    }
}

impl Target {
    /// `true` if this [`Target`] is only supported when the copy target feature is enabled.
    pub(super) fn requires_copy(&self) -> bool {
        matches!(self, Target::Job(_) | Target::CronJob(_))
    }

    /// Set the container on this target. No-op for [`Target::Targetless`].
    pub fn set_container(&mut self, container: String) {
        match self {
            Target::Deployment(t) => t.container = Some(container),
            Target::Pod(t) => t.container = Some(container),
            Target::Rollout(t) => t.container = Some(container),
            Target::StatefulSet(t) => t.container = Some(container),
            Target::Service(t) => t.container = Some(container),
            Target::ReplicaSet(t) => t.container = Some(container),
            Target::Job(t) => t.container = Some(container),
            Target::CronJob(t) => t.container = Some(container),
            Target::Targetless => {}
        }
    }

    /// `true` if this [`Target`] is only supported when the operator is enabled.
    pub fn requires_operator(&self) -> bool {
        matches!(
            self,
            Target::Job(_)
                | Target::CronJob(_)
                | Target::StatefulSet(_)
                | Target::Service(_)
                | Target::ReplicaSet(_)
        )
    }
}

impl fmt::Display for TargetType {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let stringified = match self {
            TargetType::Targetless => "targetless",
            TargetType::Pod => "pod",
            TargetType::Deployment => "deployment",
            TargetType::Rollout => "rollout",
            TargetType::Job => "job",
            TargetType::CronJob => "cronjob",
            TargetType::StatefulSet => "statefulset",
            TargetType::Service => "service",
            TargetType::ReplicaSet => "replicaset",
        };

        f.write_str(stringified)
    }
}

impl TargetType {
    pub fn all() -> impl Iterator<Item = Self> {
        [
            Self::Targetless,
            Self::Pod,
            Self::Deployment,
            Self::Rollout,
            Self::Job,
            Self::CronJob,
            Self::StatefulSet,
            Self::Service,
            Self::ReplicaSet,
        ]
        .into_iter()
    }

    pub fn compatible_with(&self, config: &FeatureConfig) -> bool {
        match self {
            Self::Targetless | Self::Rollout => !config.copy_target.enabled,
            Self::Pod => !(config.copy_target.enabled && config.copy_target.scale_down),
            Self::Job | Self::CronJob => config.copy_target.enabled,
            Self::Service => !config.copy_target.enabled,
            Self::Deployment | Self::StatefulSet | Self::ReplicaSet => true,
        }
    }
}

/// Trait used to convert different aspects of a [`Target`] into a string.
///
/// It's mainly implemented using the `impl_target_display` macro, except for [`Target`]
/// and `TargetHandle`, which manually implement this.
pub trait TargetDisplay {
    /// The string version of a [`Target`]'s type, e.g. `Pod` -> `"pod"`, `StatefulSet` ->
    /// `"statefulset"`.
    fn type_(&self) -> &str;

    /// The `name` of a [`Target`], e.g. `"pod-of-beans"`.
    fn name(&self) -> &str;

    /// The optional name of a [`Target`]'s container, e.g. `"can-of-beans"`.
    fn container(&self) -> Option<&String>;
}

/// Implements the [`TargetDisplay`] and [`fmt::Display`] traits for a target type.
macro_rules! impl_target_display {
    ($struct_name:ident, $target_type:ident, $target_type_display:literal) => {
        impl TargetDisplay for $struct_name {
            fn type_(&self) -> &str {
                $target_type_display
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

impl_target_display!(PodTarget, pod, "pod");
impl_target_display!(DeploymentTarget, deployment, "deployment");
impl_target_display!(RolloutTarget, rollout, "rollout");
impl_target_display!(JobTarget, job, "job");
impl_target_display!(CronJobTarget, cron_job, "cronjob");
impl_target_display!(StatefulSetTarget, stateful_set, "statefulset");
impl_target_display!(ServiceTarget, service, "service");
impl_target_display!(ReplicaSetTarget, replica_set, "replicaset");

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
            Target::Service(target) => target.fmt(f),
            Target::ReplicaSet(target) => target.fmt(f),
        }
    }
}

impl TargetDisplay for Target {
    fn type_(&self) -> &str {
        match self {
            Target::Targetless => "targetless",
            Target::Deployment(target) => target.type_(),
            Target::Pod(target) => target.type_(),
            Target::Rollout(target) => target.type_(),
            Target::Job(target) => target.type_(),
            Target::CronJob(target) => target.type_(),
            Target::StatefulSet(target) => target.type_(),
            Target::Service(target) => target.type_(),
            Target::ReplicaSet(target) => target.type_(),
        }
    }

    fn name(&self) -> &str {
        match self {
            Target::Targetless => "targetless",
            Target::Deployment(target) => target.name(),
            Target::Pod(target) => target.name(),
            Target::Rollout(target) => target.name(),
            Target::Job(target) => target.name(),
            Target::CronJob(target) => target.name(),
            Target::StatefulSet(target) => target.name(),
            Target::Service(target) => target.name(),
            Target::ReplicaSet(target) => target.name(),
        }
    }

    fn container(&self) -> Option<&String> {
        match self {
            Target::Targetless => None,
            Target::Deployment(target) => target.container(),
            Target::Pod(target) => target.container(),
            Target::Rollout(target) => target.container(),
            Target::Job(target) => target.container(),
            Target::CronJob(target) => target.container(),
            Target::StatefulSet(target) => target.container(),
            Target::Service(target) => target.container(),
            Target::ReplicaSet(target) => target.container(),
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
        const SERVICE = 256;
        const REPLICA_SET = 512;
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
                Target::Service(target) => {
                    flags |= TargetAnalyticFlags::SERVICE;
                    if target.container.is_some() {
                        flags |= TargetAnalyticFlags::CONTAINER;
                    }
                }
                Target::ReplicaSet(target) => {
                    flags |= TargetAnalyticFlags::REPLICA_SET;
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
    use crate::config::{ConfigContext, MirrordConfig};

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
        let mut cfg_context = ConfigContext::default()
            .override_env_opt("MIRRORD_IMPERSONATED_TARGET", path_env)
            .override_env_opt("MIRRORD_TARGET_NAMESPACE", namespace_env)
            .strict_env(true);
        let generated_target_config = TargetFileConfig::default()
            .generate_config(&mut cfg_context)
            .unwrap();

        assert_eq!(expected_target_config, generated_target_config);
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
        let mut cfg_context = ConfigContext::default().strict_env(true);
        let target_file_config: TargetFileConfig =
            serde_json::from_str(config_json_string).unwrap();
        let target_config: TargetConfig = target_file_config
            .generate_config(&mut cfg_context)
            .unwrap();
        assert_eq!(target_config, expected_target_config);

        // Now test that the namespace is set (overridden) by the env var.
        let namespace = "override-namespace";
        expected_target_config.namespace = Some(namespace.to_string());
        let mut cfg_context = ConfigContext::default()
            .override_env("MIRRORD_TARGET_NAMESPACE", namespace)
            .strict_env(true);
        let target_file_config: TargetFileConfig =
            serde_json::from_str(config_json_string).unwrap();
        let target_config: TargetConfig = target_file_config
            .generate_config(&mut cfg_context)
            .unwrap();
        assert_eq!(target_config, expected_target_config);
    }
}
