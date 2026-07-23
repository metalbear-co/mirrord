use std::collections::BTreeMap;

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::PreviewSessionPhase;

/// Read-only view of a preview environment, served by the operator's preview status API
/// (`GET /apis/operator.metalbear.co/v1/previews`).
///
/// This is served from the operator's aggregated API instead of being a stored Kubernetes
/// object because the view joins state that lives in DIFFERENT clusters: the primary's
/// `PreviewSession` CR plus the phase of every workload cluster's replica copy. Storing that
/// as a real CR would mean the operator continuously writing a synchronized summary object
/// and keeping it fresh across cluster outages - a cache that is stale exactly when it
/// matters (a cluster stops responding). Answering at request time from the live
/// `PreviewSession` CRs means there is nothing to synchronize or invalidate, and clients
/// always see the current truth including `Unreachable`/`Missing` clusters. Clients talking
/// to an operator without this route just get a 404.
///
/// One entry per logical preview: multicluster replica copies are folded into their
/// primary's entry, never listed. The `CustomResource` derive is used only to get the kube
/// `Resource` impl (group/version/plural) and a `metadata`-carrying wrapper; `plural` is
/// pinned to `previews` because it is the wire route.
#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "operator.metalbear.co",
    version = "v1",
    kind = "PreviewSessionView",
    plural = "previews",
    namespaced,
    status = "PreviewSessionViewStatus"
)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSessionViewSpec {
    /// The user-facing preview key (shared by every cluster's copy).
    pub key: String,
    /// Target workload, e.g. `deployment/my-app`.
    pub target: String,
    /// The user's container image running in the preview pods.
    pub image: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSessionViewStatus {
    /// Lifecycle phase of the preview.
    pub phase: PreviewSessionPhase,
    /// The most important thing to know about this preview beyond its phase, when there is
    /// one: why it failed, or why it is running in a reduced form.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<PreviewMessage>,
    /// Per-workload-cluster replica phase, aggregated live by the multicluster primary
    /// (empty on single-cluster operators). Values are phase names plus the aggregation-only
    /// `Unreachable` (the cluster's copy could not be queried) and `Missing` (the copy does
    /// not exist yet), which is why this stays stringly-typed.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub clusters: BTreeMap<String, String>,
}

/// A message about a preview, with the severity the client should present it at.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PreviewMessage {
    pub kind: PreviewMessageKind,
    pub text: String,
}

/// What a [`PreviewMessage`] means for the preview.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
pub enum PreviewMessageKind {
    /// The preview failed; `text` carries the failure detail.
    Failure,
    /// The preview serves in a reduced form (e.g. pods only on the default cluster because
    /// replicas are disabled or the branch-proxy credential is unavailable).
    Degraded,
    /// A kind this client version does not know - newer operators may add kinds, and an
    /// unknown one must not break deserialization.
    #[serde(other)]
    Unknown,
}
