use kube::CustomResource;
use mirrord_config::{feature::split_queues::SplitQueuesConfig, target::Target};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::crd::Session;

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
    /// The pod is deleted when this timeout has expired and there are no connected clients.
    pub idle_ttl: Option<u32>,
    /// Should the operator scale down target deployment to 0 while this pod is alive.
    /// Ignored if [`Target`] is [`Target::Pod`].
    pub scale_down: bool,
    /// Split queues client side configuration.
    #[schemars(schema_with = "split_queues_schema")]
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

/// Generates a permissive schema for [`CopyTargetSpec::split_queues`].
///
/// The config accepts both the map form (keyed by queue id) and the list form (entries that carry
/// their own id, so the same id can repeat across brokers). A Kubernetes structural schema cannot
/// describe "either an object or an array", so we store it opaquely with
/// `x-kubernetes-preserve-unknown-fields`: the API server keeps whatever shape the client sent and
/// the operator validates it after deserializing.
fn split_queues_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    let mut schema = schemars::json_schema!({ "nullable": true });
    schema.insert(
        "x-kubernetes-preserve-unknown-fields".to_owned(),
        serde_json::Value::Bool(true),
    );
    schema
}
