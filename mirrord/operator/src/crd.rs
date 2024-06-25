use kube::CustomResource;
use mirrord_config::target::{Target, TargetConfig, TargetDisplay};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use self::label_selector::LabelSelector;
use crate::types::LicenseInfoOwned;

pub mod label_selector;

pub const TARGETLESS_TARGET_NAME: &str = "targetless";

#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "operator.metalbear.co",
    version = "v1",
    kind = "Target",
    root = "TargetCrd",
    namespaced
)]
pub struct TargetSpec {
    /// None when targetless.
    pub target: CompatTarget,
}

#[derive(Clone, Eq, PartialEq, Hash, Debug, JsonSchema)]
#[serde(transparent, deny_unknown_fields)]
pub struct CompatTarget(pub Target);

impl core::fmt::Display for CompatTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt_display(f)
    }
}

impl core::ops::Deref for CompatTarget {
    type Target = Target;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<Target> for CompatTarget {
    fn from(value: Target) -> Self {
        Self(value)
    }
}

impl Serialize for CompatTarget {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for CompatTarget {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let deserialized = serde_json::Value::deserialize(deserializer)?;
        println!("deserialized \n\n{deserialized:?}");
        let target_name = deserialized.get("name").map(ToString::to_string);

        let target = serde_json::from_value(deserialized);
        println!("as target \n\n{target:?}");
        match target {
            Ok(target) => Ok(CompatTarget(target)),
            Err(_) => Ok(CompatTarget(Target::Unknown(
                target_name.unwrap_or_default(),
            ))),
        }
    }
}

impl TargetCrd {
    /// Creates target name in format of target_type.target_name.[container.container_name]
    /// for example:
    /// deploy.nginx
    /// deploy.nginx.container.nginx
    #[tracing::instrument(level = "info", ret)]
    pub fn target_name(target: &Target) -> String {
        let (type_name, target, container) = match target {
            Target::Deployment(target) => ("deploy", &target.deployment, &target.container),
            Target::Pod(target) => ("pod", &target.pod, &target.container),
            Target::Rollout(target) => ("rollout", &target.rollout, &target.container),
            Target::Job(target) => ("job", &target.job, &target.container),
            Target::CronJob(target) => ("cronjob", &target.cron_job, &target.container),
            Target::StatefulSet(target) => ("statefulset", &target.stateful_set, &target.container),
            Target::Targetless => return TARGETLESS_TARGET_NAME.to_string(),
            Target::Unknown(target) => {
                panic!("UNKNOWN");
                return target.clone();
            }
        };

        tracing::info!("type {type_name} target {target} container {container:?}");

        if let Some(container) = container {
            format!("{}.{}.container.{}", type_name, target, container)
        } else {
            format!("{}.{}", type_name, target)
        }
    }

    /// "targetless" ([`TARGETLESS_TARGET_NAME`]) if `None`,
    /// else <resource_type>.<resource_name>...
    #[tracing::instrument(level = "debug", ret)]
    pub fn target_name_by_config(target_config: &TargetConfig) -> String {
        target_config
            .path
            .as_ref()
            .map_or_else(|| TARGETLESS_TARGET_NAME.to_string(), Self::target_name)
    }

    #[tracing::instrument(level = "info", ret)]
    pub fn type_dot_name(&self) -> String {
        println!("\nthe spec is {}", self.spec.target);
        match &self.spec.target.0 {
            Target::Deployment(target) => format!("deploy.{target}"),
            Target::Pod(target) => format!("pod.{target}"),
            Target::Rollout(target) => format!("rollout.{target}"),
            Target::Job(target) => format!("job.{target}"),
            Target::CronJob(target) => format!("cronjob.{target}"),
            Target::StatefulSet(target) => format!("statefulset.{target}"),
            Target::Targetless => "targetless".to_string(),
            Target::Unknown(target) => format!("unknown.{target}"),
        }
    }
}

impl From<TargetCrd> for TargetConfig {
    fn from(crd: TargetCrd) -> Self {
        TargetConfig {
            path: Some(crd.spec.target.0),
            namespace: crd.metadata.namespace,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
pub struct TargetPortLock {
    pub target_hash: String,
    pub port: u16,
}

pub static OPERATOR_STATUS_NAME: &str = "operator";

#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "operator.metalbear.co",
    version = "v1",
    kind = "MirrordOperator",
    root = "MirrordOperatorCrd",
    status = "MirrordOperatorStatus"
)]
pub struct MirrordOperatorSpec {
    pub operator_version: String,
    pub default_namespace: String,
    pub features: Option<Vec<OperatorFeatures>>,
    pub license: LicenseInfoOwned,
    pub protocol_version: Option<String>,
    pub copy_target_enabled: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
pub struct MirrordOperatorStatus {
    pub sessions: Vec<Session>,
    pub statistics: Option<MirrordOperatorStatusStatistics>,

    /// Option because added later.
    /// (copy-target pod name, copy-target resource)
    pub copy_targets: Option<Vec<(String, CopyTargetCrd)>>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
pub struct MirrordOperatorStatusStatistics {
    pub dau: usize,
    pub mau: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct Session {
    pub id: Option<String>,
    pub duration_secs: u64,
    pub user: String,
    pub target: String,
    pub namespace: Option<String>,
    pub locked_ports: Option<Vec<(u16, String, Option<String>)>>,
}

/// Resource used to access the operator's session management routes.
///
/// - `kind = Session` controls how [`kube`] generates the route, in this case it becomes
///   `/sessions`;
/// - `root = "SessionCrd"` is the json return value we get from this resource's API;
/// - `SessionSpec` itself contains the custom data we want to pass in the the response, which in
///   this case is nothing;
///
/// The [`SessionCrd`] is used to provide the k8s_openapi `APIResource`, see `API_RESOURCE_LIST` in
/// the operator.
#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "operator.metalbear.co",
    version = "v1",
    kind = "Session",
    root = "SessionCrd"
)]
pub struct SessionSpec;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
pub enum OperatorFeatures {
    ProxyApi,
}

/// This [`Resource`](kube::Resource) represents a copy pod created from an existing [`Target`]
/// (operator's copy pod feature).
#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "operator.metalbear.co",
    version = "v1",
    kind = "CopyTarget",
    root = "CopyTargetCrd",
    namespaced
)]
pub struct CopyTargetSpec {
    /// Original target. Only [`Target::Pod`] and [`Target::Deployment`] are accepted.
    pub target: Target,
    /// How long should the operator keep this pod alive after its creation.
    /// The pod is deleted when this timout has expired and there are no connected clients.
    pub idle_ttl: Option<u32>,
    /// Should the operator scale down target deployment to 0 while this pod is alive.
    /// Ignored if [`Target`] is not [`Target::Deployment`].
    pub scale_down: bool,
}

/// Features and operations that can be blocked by a `MirrordPolicy`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")] // StealWithoutFilter -> steal-without-filter in yaml.
pub enum BlockedFeature {
    /// Blocks stealing traffic in any way (without or without filter).
    Steal,
    /// Blocks stealing traffic without specifying (any) filter. Client can still specify a
    /// filter that matches anything.
    StealWithoutFilter,
}

/// Custom resource for policies that limit what mirrord features users can use.
#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    // The operator group is handled by the operator, we want policies to be handled by k8s.
    group = "policies.mirrord.metalbear.co",
    version = "v1alpha",
    kind = "MirrordPolicy",
    namespaced
)]
#[serde(rename_all = "camelCase")] // target_path -> targetPath in yaml.
pub struct MirrordPolicySpec {
    /// Specify the targets for which this policy applies, in the pod/my-pod deploy/my-deploy
    /// notation. Targets can be matched using `*` and `?` where `?` matches exactly one
    /// occurrence of any character and `*` matches arbitrary many (including zero) occurrences
    /// of any character. If not specified, this policy does not depend on the target's path.
    pub target_path: Option<String>,

    /// If specified in a policy, the policy will only apply to targets with labels that match all
    /// of the selector's rules.
    pub selector: Option<LabelSelector>,

    // TODO: make the k8s list type be set/map to prevent duplicates.
    /// List of features and operations blocked by this policy.
    pub block: Vec<BlockedFeature>,
}
