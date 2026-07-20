use std::collections::BTreeMap;

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Read-only view of a preview environment, served by the operator's preview status API
/// (`GET /apis/operator.metalbear.co/v1/previews`).
///
/// This is not a stored Kubernetes object and there is no `CustomResourceDefinition` for it.
/// The whole `operator.metalbear.co` group is served through the operator's aggregated API, so
/// every request is answered live from the `PreviewSession` CRs: one entry per logical preview
/// (multicluster replica copies are folded in, never listed), and on a multicluster primary the
/// per-cluster replica phases are aggregated by reading each workload cluster's copy at request
/// time. Statuses stay in the copies' own status subresources; nothing here is persisted. The
/// `CustomResource` derive is used only to get the kube `Resource` impl (group/version/kind/
/// plural) and a `metadata`-carrying wrapper.
#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "operator.metalbear.co",
    version = "v1",
    kind = "Preview",
    namespaced,
    status = "PreviewStatus"
)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSpec {
    /// The user-facing preview key (shared by every cluster's copy).
    pub key: String,
    /// Target workload, e.g. `deployment/my-app`.
    pub target: String,
    /// The user's container image running in the preview pods.
    pub image: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PreviewStatus {
    /// Lifecycle phase of the preview, e.g. `Ready` or `Failed`.
    pub phase: String,
    /// The most important thing to know about this preview beyond its phase, when there is
    /// one: why it failed, or why it is running in a reduced form.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<PreviewMessage>,
    /// Per-workload-cluster replica phase, aggregated live by the multicluster primary
    /// (empty on single-cluster operators). `Unreachable` marks a cluster whose copy could
    /// not be queried, `Missing` one where the copy does not exist (yet).
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
