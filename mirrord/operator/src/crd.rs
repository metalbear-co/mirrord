use std::{
    collections::HashMap,
    fmt::{Display, Formatter},
};
use std::collections::{BTreeMap, BTreeSet};

use k8s_openapi::api::core::v1::{PodSpec, PodTemplateSpec};
use kube::CustomResource;
pub use mirrord_config::feature::split_queues::QueueId;
use mirrord_config::{
    feature::split_queues::SqsMessageFilter,
    target::{Target, TargetConfig},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
#[cfg(feature = "client")]
use crate::client::OperatorApiError;

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
    pub target: Option<Target>,
    pub port_locks: Option<Vec<TargetPortLock>>,
}

impl TargetCrd {
    /// Creates target name in format of target_type.target_name.[container.container_name]
    /// for example:
    /// deploy.nginx
    /// deploy.nginx.container.nginx
    pub fn target_name(target: &Target) -> String {
        let (type_name, target, container) = match target {
            Target::Deployment(target) => ("deploy", &target.deployment, &target.container),
            Target::Pod(target) => ("pod", &target.pod, &target.container),
            Target::Rollout(target) => ("rollout", &target.rollout, &target.container),
            Target::Targetless => return TARGETLESS_TARGET_NAME.to_string(),
        };
        if let Some(container) = container {
            format!("{}.{}.container.{}", type_name, target, container)
        } else {
            format!("{}.{}", type_name, target)
        }
    }

    /// "targetless" ([`TARGETLESS_TARGET_NAME`]) if `None`,
    /// else <resource_type>.<resource_name>...
    pub fn target_name_by_config(target_config: &TargetConfig) -> String {
        target_config
            .path
            .as_ref()
            .map_or_else(|| TARGETLESS_TARGET_NAME.to_string(), Self::target_name)
    }

    pub fn name(&self) -> String {
        self.spec
            .target
            .as_ref()
            .map(Self::target_name)
            .unwrap_or(TARGETLESS_TARGET_NAME.to_string())
    }
}

impl From<TargetCrd> for TargetConfig {
    fn from(crd: TargetCrd) -> Self {
        TargetConfig {
            path: crd.spec.target,
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
    /// Should be removed when we can stop supporting compatibility with versions from before the
    /// `supported_features` field was added.
    /// "Breaking" that compatibility by removing this field and then running with one old (from
    /// before the `supported_features` field) side (client or operator) would make the client
    /// think `ProxyApi` is not supported even if it is.
    #[deprecated(note = "use supported_features instead")]
    features: Option<Vec<OperatorFeatures>>,
    /// Replaces both `features` and `copy_target_enabled`. Operator versions that use a version
    /// of this code that has both this and the old fields are expected to populate this field with
    /// the full set of features they support, and the old fields with their limited info they
    /// support, for old clients.
    ///
    /// Access this info only via `supported_features()`.
    /// Optional for backwards compatibility (new clients can talk to old operators that don't send
    /// this field).
    supported_features: Option<Vec<NewOperatorFeature>>,
    pub license: LicenseInfoOwned,
    pub protocol_version: Option<String>,
    /// Should be removed when we can stop supporting compatibility with versions from before the
    /// `supported_features` field was added.
    /// "Breaking" that compatibility by removing this field and then running with one old (from
    /// before the `supported_features` field) side (client or operator) would make the client
    /// think copy target is not enabled even if it is.
    /// Optional for backwards compatibility (new clients can talk to old operators that don't send
    /// this field).
    #[deprecated(note = "use supported_features instead")]
    copy_target_enabled: Option<bool>,
}

impl MirrordOperatorSpec {
    pub fn new(
        operator_version: String,
        default_namespace: String,
        supported_features: Vec<NewOperatorFeature>,
        license: LicenseInfoOwned,
        protocol_version: Option<String>,
    ) -> Self {
        let features = supported_features
            .contains(&NewOperatorFeature::ProxyApi)
            .then(|| vec![OperatorFeatures::ProxyApi]);
        let copy_target_enabled =
            Some(supported_features.contains(&NewOperatorFeature::CopyTarget));
        #[allow(deprecated)] // deprecated objects must still be included in construction.
        Self {
            operator_version,
            default_namespace,
            supported_features: Some(supported_features),
            license,
            protocol_version,
            features,
            copy_target_enabled,
        }
    }

    /// Get a vector with the features the operator supports.
    /// Handles objects sent from old and new operators.
    // When the deprecated fields are removed, this can be changed to just return
    // `self.supported_features.unwrap_or_default()`.
    pub fn supported_features(&self) -> Vec<NewOperatorFeature> {
        self.supported_features
            .clone()
            // if supported_features was sent, just use that. If not we are dealing with an older
            // operator, so we build a vector of new features from the old fields.
            .or_else(|| {
                // object was sent by an old operator that still uses fields that are now deprecated
                #[allow(deprecated)]
                self.features.as_ref().map(|features| {
                    features
                        .iter()
                        .map(From::from)
                        .chain(
                            self.copy_target_enabled.and_then(|enabled| {
                                enabled.then_some(NewOperatorFeature::CopyTarget)
                            }),
                        )
                        .collect()
                })
            })
            // Convert `None` to empty vector since we don't expect this to often be
            // `None` (although it's ok if it is) and that way the return type is simpler.
            .unwrap_or_default()
    }

    #[cfg(feature = "client")]
    pub fn require_feature(&self, feature: NewOperatorFeature) -> Result<(), OperatorApiError> {
        if self.supported_features().contains(&feature) {
            Ok(())
        } else {
            Err(OperatorApiError::UnsupportedFeature {
                feature,
                operator_version: self.operator_version.clone(),
            })
        }
    }
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

/// Since this enum does not have a variant marked with `#[serde(other)]`, and is present like that
/// in released clients, existing clients would fail to parse any new variant. This means the
/// operator can never send anything but the one existing variant, otherwise the client will error
/// out.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
pub enum OperatorFeatures {
    ProxyApi,
    // DON'T ADD VARIANTS - old clients won't be able to deserialize them.
    // Add new features in NewOperatorFeature
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
pub enum NewOperatorFeature {
    ProxyApi,
    CopyTarget,
    Sqs,
    SessionManagement,
    /// This variant is what a client sees when the operator includes a feature the client is not
    /// yet aware of, because it was introduced in a version newer than the client's.
    #[serde(other)]
    FeatureFromTheFuture,
}

impl Display for NewOperatorFeature {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            NewOperatorFeature::ProxyApi => "proxy API",
            NewOperatorFeature::CopyTarget => "copy target",
            NewOperatorFeature::Sqs => "SQS queue splitting",
            NewOperatorFeature::FeatureFromTheFuture => "unknown feature",
            NewOperatorFeature::SessionManagement => "session management",
        };
        f.write_str(name)
    }
}

impl From<&OperatorFeatures> for NewOperatorFeature {
    fn from(old_feature: &OperatorFeatures) -> Self {
        match old_feature {
            OperatorFeatures::ProxyApi => NewOperatorFeature::ProxyApi,
        }
    }
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
    /// queue id -> (attribute name -> regex)
    pub sqs_filter: Option<HashMap<QueueId, SqsMessageFilter>>,
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")] // queue_name_key -> queueNameKey in yaml.
pub struct ConfigMapQueueNameSource {
    /// The name of the config map that holds the name of the queue we want to split.
    pub name: String,

    /// The name of the key in the config map that holds the name of the queue we want to
    /// split.
    pub queue_name_key: String,
}

/// Set where the application reads the name of the queue from, so that mirrord can find that queue,
/// split it, and temporarily change the name there to the name of the branch queue when splitting.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")] // ConfigMap -> configMap in yaml.
pub enum QueueNameSource {
    ConfigMap(ConfigMapQueueNameSource),
    EnvVar(String),
}

pub type OutputQueueName = String;

/// The details of a queue that should be split.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(tag = "queueType")]
// So that controllers that only handle 1 type of queue don't have to adapt when we add more queue types.
#[non_exhaustive]
pub enum SplitQueue {
    /// Amazon SQS
    ///
    /// Where the application gets the queue name from. Will be used to read messages from that
    /// queue and distribute them to the output queues. When running with mirrord and splitting
    /// this queue, applications will get a modified name from that source.
    #[serde(rename = "SQS")]
    Sqs(QueueNameSource),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")] // Deployment -> deployment in yaml.
pub enum QueueConsumer {
    Deployment(String),
    Rollout(String),
}

impl QueueConsumer {
    pub fn get_type_and_name(&self) -> (&str, &str) {
        match self {
            QueueConsumer::Deployment(dep) => ("deployment", dep),
            QueueConsumer::Rollout(roll) => ("rollout", roll)
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
pub struct Filtering {
    pub original_spec: PodTemplateSpec,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
pub struct QueueNameUpdate {
    pub original_name: String,
    pub output_name: String,
}


/// Details retrieved from K8s resources once the splitter is active, used on filter session
/// creation to create the altered config maps and pod specs that make the application use the
/// output queues instead of the original.
// This status struct is not optimal in that it contains redundant information. This makes the
// controller's code a bit simpler.
// The information on config map updates and on env vars is present in the resource itself, but it
// is organized differently.
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
pub struct QueueDetails {
    /// For each queue_id, the actual queue name as retrieved from the target's pod spec or config
    /// map, together with the name of its temporary output queue.
    pub queue_names: BTreeMap<QueueId, QueueNameUpdate>,

    /// Names of env vars that contain the queue name directly in the pod spec, without config
    /// map refs, mapped to their queue id.
    pub direct_env_vars: HashMap<String, QueueId>,

    /// For each config map name, a mapping from queue id to key name in the map that holds the name
    /// of that queue.
    pub config_map_updates: BTreeMap<String, HashMap<QueueId, String>>,
    //                               ^               ^        ^
    //                               |               |        ---- name of key that points to Q name
    //                               |               ---- queue id
    //                               ---- ConfigMap name

    // TODO: Don't save whole PodSpec, because its schema is so long you can't create the CRD with
    //  `kubectl apply` due to a length limit.
    /// The original PodSpec of the queue consumer.
    pub original_spec: PodSpec,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")] // active_filters -> activeFilters
pub struct QueueSplitterStatus {
    pub queue_details: QueueDetails,

    // can't be a set: "uniqueItems cannot be set to true since the runtime complexity becomes quadratic"
    /// Resource names of active sessions of this splitter.
    pub active_sessions: Vec<String>,
}

impl QueueSplitterStatus {
    pub fn output_queue_names(&self) -> Vec<&str> {
        self.queue_details.queue_names
            .values()
            .map(|QueueNameUpdate { output_name, .. }| output_name.as_str())
            .collect()
    }
}

/// Defines a Custom Resource that holds a central configuration for splitting a queue. mirrord
/// users specify a splitter by name in their configuration. mirrord then starts splitting according
/// to the spec and the user's filter.
#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
group = "splitters.mirrord.metalbear.co",
version = "v1alpha",
kind = "MirrordQueueSplitter",
shortname = "qs",
status = "QueueSplitterStatus",
namespaced
)]
pub struct MirrordQueueSplitterSpec {
    /// A map of the queues that should be split.
    /// The key is used by users to associate filters to the right queues.
    pub queues: BTreeMap<QueueId, SplitQueue>,

    /// The resource (deployment or Argo rollout) that reads from the queues.
    pub consumer: QueueConsumer,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename = "SQSSessionDetails", rename_all = "camelCase")]
pub struct SqsSessionDetails {
    // TODO: Don't save whole PodSpec, because its schema is so long you can't create the CRD with
    //  `kubectl apply` due to a length limit.
    pub spec: PodSpec,
    pub queue_names: BTreeMap<QueueId, QueueNameUpdate>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename = "SQSSessionStatus", rename_all = "camelCase")]
pub struct SqsSessionStatus {
    // TODO: is this option unnecessary?
    pub details: Option<SqsSessionDetails>,
}

impl SqsSessionStatus {
    pub fn is_ready(&self) -> bool {
        self.details.is_some()
    }
}


/// The [`kube::runtime::wait::Condition`] trait is auto-implemented for this function.
/// To be used in [`kube::runtime::wait::await_condition`].
pub fn is_session_ready(session: Option<&MirrordSqsSession>) -> bool {
    session
        .and_then(|session| session.status.as_ref())
        .map(|status| status.is_ready())
        .unwrap_or_default()
}

// TODO: docs
// TODO: Clients actually never use this resource in any way directly, so maybe we could define it
//  in the operator on startup? The operator would probably need permissions to create CRDs and to
//  give itself permissions for those CRDs? Is that possible? Right now it's the user that installs
//  the operator that defines this CRD and also give the operator permissions to do stuff with it.
#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
group = "splitters.mirrord.metalbear.co",
version = "v1alpha",
kind = "MirrordSQSSession",
root = "MirrordSqsSession", // for Rust naming conventions (Sqs, not SQS)
status = "SqsSessionStatus",
namespaced
)]
#[serde(rename_all = "camelCase")] // queue_filters -> queueFilters
pub struct MirrordSqsSessionSpec {
    pub queue_filters: HashMap<QueueId, SqsMessageFilter>,
    pub queue_consumer: QueueConsumer,
    // The Kubernetes API can't deal with 64 bit numbers (with most significant bit set)
    // so we save that field as a string even though its source is a u64
    pub session_id: String,
}
