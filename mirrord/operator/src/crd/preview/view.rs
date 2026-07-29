use std::{borrow::Cow, collections::BTreeMap, fmt};

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::PreviewSessionPhase;
use crate::crd::session::SessionTarget;

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
    /// Target workload the preview copies its pod configuration from.
    pub target: SessionTarget,
    /// The user's container image running in the preview pods.
    pub image: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSessionViewStatus {
    /// Lifecycle phase of the preview; `None` when the session has not reported a status
    /// yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<PreviewSessionPhase>,
    /// The most important thing to know about this preview beyond its phase, when there is
    /// one: why it failed, or why it is running in a reduced form.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<PreviewMessage>,
    /// Per-workload-cluster replica phase, aggregated live by the multicluster primary
    /// (empty on single-cluster operators).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub clusters: BTreeMap<String, PreviewSessionViewPhase>,
}

/// One cluster's entry in [`PreviewSessionViewStatus::clusters`]: the phase of the cluster's
/// copy, or one of the aggregation-only markers for a copy that could not be observed.
///
/// Serialized as a PLAIN string (`"Ready"`, `"Missing"`, ...) - manual serde because a
/// derived untagged representation would let [`PreviewSessionPhase`]'s `#[serde(other)]`
/// catch-all swallow the marker strings before their own variants could match.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(from = "String", into = "String")]
pub enum PreviewSessionViewPhase {
    /// The cluster's copy reports this session phase.
    Active(PreviewSessionPhase),
    /// The cluster's copies could not be queried (apiserver unreachable, credential
    /// failure).
    Unreachable,
    /// The cluster has no copy of this preview (yet).
    Missing,
}

impl From<String> for PreviewSessionViewPhase {
    fn from(value: String) -> Self {
        match value.as_str() {
            "Unreachable" => Self::Unreachable,
            "Missing" => Self::Missing,
            other => Self::Active(
                serde_json::from_value(serde_json::Value::String(other.to_owned()))
                    .unwrap_or(PreviewSessionPhase::Unknown),
            ),
        }
    }
}

impl From<PreviewSessionViewPhase> for String {
    fn from(value: PreviewSessionViewPhase) -> Self {
        value.to_string()
    }
}

impl fmt::Display for PreviewSessionViewPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Active(phase) => phase.fmt(f),
            Self::Unreachable => f.write_str("Unreachable"),
            Self::Missing => f.write_str("Missing"),
        }
    }
}

impl JsonSchema for PreviewSessionViewPhase {
    fn schema_name() -> Cow<'static, str> {
        "PreviewSessionViewPhase".into()
    }

    /// Plain string on the wire (see the type docs), so the schema is `String`'s.
    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        String::json_schema(generator)
    }
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

#[cfg(test)]
mod view_phase_wire_format {
    use super::*;

    /// The clusters map is consumed by CLIs of MIXED versions: entries must round-trip as
    /// the plain phase strings the first release served, with the marker strings taking
    /// priority over the session-phase catch-all.
    #[test]
    fn round_trips_plain_strings() {
        for (phase, wire) in [
            (
                PreviewSessionViewPhase::Active(PreviewSessionPhase::Ready),
                "\"Ready\"",
            ),
            (PreviewSessionViewPhase::Unreachable, "\"Unreachable\""),
            (PreviewSessionViewPhase::Missing, "\"Missing\""),
        ] {
            assert_eq!(serde_json::to_string(&phase).expect("serializes"), wire);
            assert_eq!(
                serde_json::from_str::<PreviewSessionViewPhase>(wire).expect("deserializes"),
                phase
            );
        }
    }

    #[test]
    fn unknown_phase_string_becomes_active_unknown() {
        assert_eq!(
            serde_json::from_str::<PreviewSessionViewPhase>("\"SomethingNew\"")
                .expect("deserializes"),
            PreviewSessionViewPhase::Active(PreviewSessionPhase::Unknown)
        );
    }
}
