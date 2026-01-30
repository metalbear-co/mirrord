use std::collections::BTreeMap;

use k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime;
use kube::CustomResource;
use mirrord_config::{feature::split_queues::SplitQueuesConfig, target::Target};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::crd::{
    Session,
    session::{SessionOwner, SessionTarget},
};

/// This resource represents a copy pod created from an existing [`Target`]
/// (operator's copy pod feature).
#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[kube(
    group = "operator.metalbear.co",
    version = "v1",
    kind = "CopyTarget",
    root = "CopyTargetCrd",
    status = "CopyTargetStatus",
    namespaced
)]
pub struct CopyTargetSpec {
    /// Original target.
    pub target: Target,
    /// How long should the operator keep this pod alive after its creation.
    /// The pod is deleted when this timout has expired and there are no connected clients.
    pub idle_ttl: Option<u32>,
    /// Should the operator scale down target deployment to 0 while this pod is alive.
    /// Ignored if [`Target`] is [`Target::Pod`].
    pub scale_down: bool,
    /// Split queues client side configuration.
    pub split_queues: Option<SplitQueuesConfig>,
    /// Containers that are ignored by copy target.
    #[serde(default)]
    pub exclude_containers: Vec<String>,
    /// Init containers that are ignored by copy target.
    #[serde(default)]
    pub exclude_init_containers: Vec<String>,
}

/// This is the `status` field for [`CopyTargetCrd`].
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct CopyTargetStatus {
    /// The session object of the original operator session that created this CopyTarget.
    pub creator_session: Session,
    /// Current phase of the copy.
    ///
    /// Either `InProgress`, `Ready`, or `Failed`.
    /// Stored as a string for some future compatibility.
    ///
    /// Not filled by older operator versions.
    pub phase: Option<String>,
    /// Optional message describing the reason for copy failure.
    ///
    /// Only set when `phase` is `Failed`.
    pub failure_message: Option<String>,
}

impl CopyTargetStatus {
    pub const PHASE_IN_PROGRESS: &'static str = "InProgress";
    pub const PHASE_READY: &'static str = "Ready";
    pub const PHASE_FAILED: &'static str = "Failed";
}

/// Persistent representation of a request to copy a session target.
///
/// Used internally in the operator, and mapped to `operator.metalbear.co/v1 CopyTarget`,
/// that is accessible to the users.
#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[kube(
    group = "mirrord.metalbear.co",
    version = "v1alpha",
    kind = "MirrordClusterCopyTargetRequest",
    status = "MirrordClusterCopyTargetRequestStatus"
)]
#[serde(rename_all = "camelCase")]
pub struct MirrordClusterCopyTargetRequestSpec {
    /// Target to copy.
    pub target: SessionTarget,
    /// Namespace of the target.
    pub namespace: String,
    /// Whether the target workload should be scaled down for the duration of the copy.
    pub scaledown: bool,
    /// ID of the session that created this request.
    pub creator_session: String,
    /// Details of the user that owned the session that created this request.
    pub creator_session_owner: SessionOwner,
    /// SQS filters to use for the copy.
    ///
    /// Empty if SQS splitting is not used.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sqs_splitting: BTreeMap<String, BTreeMap<String, String>>,
    /// Kafka filters to use for the copy.
    ///
    /// Empty if Kafka splitting is not used.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub kafka_splitting: BTreeMap<String, BTreeMap<String, String>>,
    /// Names of containers to be excluded from the copy.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_containers: Vec<String>,
    /// Names of init containers to be excluded from the copy.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_init_containers: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MirrordClusterCopyTargetRequestStatus {
    /// Marks the time when the request has finished spawning its first pod.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready_at: Option<MicroTime>,
    /// Marks the time when the request was last used by any mirrord session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub used_at: Option<MicroTime>,
    /// If the request is closed, this field contains the reason.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed: Option<MirrordClusterCopyTargetRequestClosed>,
}

/// Details of why a copy target request is closed.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MirrordClusterCopyTargetRequestClosed {
    /// Short reason name in PascalCase.
    pub reason: String,
    /// Human-friendly message.
    pub message: String,
}
