use std::collections::BTreeMap;

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::crd::{
    queue_filter::{MessageFilter, message_filter_crd_schema},
    session::{SessionOwner, SessionTarget},
};

/// Read-only view of a live queue-splitting session, served by the operator's
/// queue-splitting status API and browsed with `mirrord queues`.
///
/// This is not a stored Kubernetes object and there is no `CustomResourceDefinition`
/// for it. The whole `operator.metalbear.co` group is served through the operator's
/// aggregated API, so every request is answered live from the queue-splitting
/// controller's in-memory state. The `CustomResource` derive is used only to get
/// the kube `Resource` impl (group/version/kind/plural) and a `metadata`-carrying
/// wrapper; nothing is persisted in etcd.
#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "operator.metalbear.co",
    version = "v1",
    kind = "QueueSplit",
    namespaced,
    status = "QueueSplitStatus"
)]
#[serde(rename_all = "camelCase")]
pub struct QueueSplitSpec {
    /// mirrord session id this split belongs to, in the same uppercase hex
    /// format the CLI shows. Matches the prefix of the backing split session.
    pub session: String,
    /// Target workload being split.
    pub target: SessionTarget,
    /// The developer who started the session.
    pub owner: SessionOwner,
    /// Queue filters as requested in the user's mirrord config.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filters: Vec<QueueSplitFilter>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueueSplitFilter {
    /// Queue id from the user's mirrord config.
    pub id: String,
    /// Broker type, e.g. `SQS` or `Kafka`.
    pub queue_type: String,
    /// Header/attribute regex filters: attribute name -> regex pattern. Set when the session
    /// was requested with the legacy `message_filter` map.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub message_filter: BTreeMap<String, String>,
    /// The composable filter tree. Set when the session was requested with the `filter` shape;
    /// `message_filter` is empty then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "message_filter_crd_schema")]
    pub filter: Option<MessageFilter>,
    /// Optional jq filter applied to the structured message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jq_filter: Option<String>,
}

impl QueueSplitFilter {
    /// The requested filter as one tree, whichever shape it was requested in. `None` when the
    /// queue has no attribute filter at all (jq only, or nothing).
    pub fn message_filter(&self) -> Option<MessageFilter> {
        self.filter
            .clone()
            .or_else(|| (!self.message_filter.is_empty()).then(|| (&self.message_filter).into()))
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueueSplitStatus {
    /// Lifecycle phase: `Pending`, `Ready`, or `Failed`.
    pub phase: String,
    /// Human friendly detail about the current phase, such as a failure reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Queues found in the target, one or more per requested filter.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub queues: Vec<QueueSplitQueue>,
    /// Target pods seen for this session and whether each is patched and ready.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub target_pods: Vec<QueueSplitTargetPod>,
    /// Temporary broker resources mirrord created for this session.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tmp_queues: Vec<QueueSplitTmpQueue>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueueSplitQueue {
    /// Filter id this queue was resolved from.
    pub id: String,
    /// Broker type, e.g. `SQS` or `Kafka`.
    #[serde(rename = "type")]
    pub queue_type: String,
    /// Original SQS / Azure Service Bus queue name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<String>,
    /// Original topic name (Kafka, GCP Pub/Sub, Azure Service Bus).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
    /// Original consumer group (Kafka).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consumer_group: Option<String>,
    /// Original subscription (GCP Pub/Sub, Azure Service Bus).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription: Option<String>,
}

/// One temporary broker resource mirrord created for a session, such as the
/// `mirrord-tmp-` prefixed SQS queue the local process reads from. Sessions are
/// invisible in the broker itself, so this is what lets a user trace a resource
/// they see there back to the session that owns it.
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueueSplitTmpQueue {
    /// Filter id this resource was created for.
    pub id: String,
    /// Broker type, e.g. `SQS` or `Kafka`.
    #[serde(rename = "type")]
    pub queue_type: String,
    /// Shape of the resource in the broker: `queue`, `topic`, `subscription`,
    /// `channel`, `taskQueue` or `consumerGroup`. A single broker can create
    /// more than one shape per session (GCP Pub/Sub creates both a topic and a
    /// subscription).
    pub kind: String,
    /// Who the resource belongs to: `workload` for the ones the whole split
    /// shares, which the target's pods read from, or `session` for the ones
    /// created for this session alone. Every session on the same target reports
    /// the same `workload` resources.
    pub scope: String,
    /// Name of the original resource this one temporarily stands in for.
    pub original: String,
    /// Name of the temporary resource.
    pub name: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueueSplitTargetPod {
    pub name: String,
    /// The pod carries the split's env-var patch.
    pub patched: bool,
    /// The pod is in the `Running` phase with all containers ready.
    pub ready: bool,
}
