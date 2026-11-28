use std::collections::BTreeMap;

use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
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
    /// Original queues the operator has stopped reading for now, and what each waits on.
    /// Only Temporal reports these: one entry per task queue and task kind whose polls are
    /// paused because a virtual task queue's buffer is full. Older operators never send the
    /// field.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub held_queues: Vec<QueueSplitHeldQueue>,
}

/// One original queue the operator is not reading right now because a buffer it fills is
/// full. Every session on the same original queue reports the same entry: the hold stops
/// tasks for all of them, not only for the session whose buffer is full.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueueSplitHeldQueue {
    /// Filter id the held queue was resolved for.
    pub id: String,
    /// Broker type, `Temporal`.
    #[serde(rename = "type")]
    pub queue_type: String,
    /// The original task queue the operator is not polling.
    pub queue: String,
    /// Which tasks are held: `workflow` or `activity`.
    pub task_kind: String,
    /// The virtual task queue whose buffer is full. Matches the `name` of a `tmpQueues` entry
    /// of this split or of another session's split on the same target, whose `scope` says
    /// whether the deployed workload or a session reads it.
    pub full_queue: String,
    /// When the hold started.
    pub since: Time,
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
    /// What keeps the pod from being ready.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[cfg(test)]
mod held_queues_wire_format {
    use k8s_openapi::jiff::Timestamp;

    use super::*;

    /// Released operators serve statuses without `heldQueues`, and released CLIs read statuses
    /// that carry it: a status without the field parses to an empty list, the field round-trips
    /// in camelCase, and an empty list is left out so an older CLI sees the status it knows.
    #[test]
    fn status_without_held_queues_parses_and_empty_list_is_not_sent() {
        let old = serde_json::json!({ "phase": "Ready" });
        let status: QueueSplitStatus = serde_json::from_value(old).expect("deserializes");
        assert!(status.held_queues.is_empty());
        assert!(
            serde_json::to_value(&status)
                .expect("serializes")
                .get("heldQueues")
                .is_none()
        );

        let status = QueueSplitStatus {
            phase: "Ready".to_owned(),
            held_queues: vec![QueueSplitHeldQueue {
                id: "orders".to_owned(),
                queue_type: "Temporal".to_owned(),
                queue: "order-checkout".to_owned(),
                task_kind: "workflow".to_owned(),
                full_queue: "mirrord-tmp-a-main-order-checkout".to_owned(),
                since: Time(Timestamp::from_second(1_795_860_902).expect("valid timestamp")),
            }],
            ..Default::default()
        };
        let wire = serde_json::to_value(&status).expect("serializes");
        assert_eq!(
            wire.get("heldQueues"),
            Some(&serde_json::json!([{
                "id": "orders",
                "type": "Temporal",
                "queue": "order-checkout",
                "taskKind": "workflow",
                "fullQueue": "mirrord-tmp-a-main-order-checkout",
                "since": "2026-11-28T10:15:02Z",
            }]))
        );
        let parsed: QueueSplitStatus = serde_json::from_value(wire).expect("deserializes");
        let since = parsed.held_queues.first().map(|held| held.since.clone());
        assert_eq!(
            since,
            status.held_queues.first().map(|held| held.since.clone())
        );
        assert!(since.is_some());
    }
}
