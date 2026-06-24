use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Read-only view of a live queue-splitting session, served by the operator's
/// queue-splitting status API and browsed with `mirrord queues`.
///
/// This is not a stored Kubernetes object. The operator builds it on the fly
/// from the queue-splitting controller's in-memory state, so a user can tell
/// when their split is actually live (a patched target pod is running and
/// ready) instead of guessing from how fast the session started.
#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "operator.metalbear.co",
    version = "v1",
    kind = "QueueSplit",
    root = "QueueSplitCrd"
)]
#[serde(rename_all = "camelCase")]
pub struct QueueSplitSpec {
    /// Name of the `MirrordClusterSplitSession` this view describes.
    pub session_name: String,
    /// Namespace of the target workload being split.
    pub namespace: String,
    /// Target workload, formatted as `kind/name`.
    pub target: String,
    /// Lifecycle phase: `Starting`, `ResourcesReady`, `Ready`, or `Error`.
    pub phase: String,
    /// True once the split is operational: at least one patched target pod is
    /// running and ready, so messages are being routed to the local session.
    pub ready: bool,
    /// Queue ids being split in this session.
    #[serde(default)]
    pub queues: Vec<String>,
    /// Target pods seen for this session and whether each is patched and ready.
    #[serde(default)]
    pub target_pods: Vec<QueueSplitTargetPod>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueueSplitTargetPod {
    pub name: String,
    /// The pod carries the split's env-var patch.
    pub patched: bool,
    /// The pod is in the `Running` phase with all containers ready.
    pub ready: bool,
}
