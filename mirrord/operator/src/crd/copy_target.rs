use std::borrow::Cow;

use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
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
    status = "CopyTargetStatusCompat",
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
    /// When set to `true`, `split_queues` contains wildcard config for all queue kinds.
    /// Resource creation handler must dismiss unsupported kinds rather than rejecting the
    /// creation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_queue_splitting: Option<bool>,
    /// Session key stamped onto messages matched by this copy's split session, and used for the
    /// session's message events.
    ///
    /// The operator creates the split session while handling this resource's creation, before any
    /// client connects with its `ConnectParams`, so the key has to ride in the spec. Only
    /// user-provided keys belong here: auto-generated keys differ on every run, and a per-run
    /// value in the spec would defeat the spec-equality check that lets clients reuse an existing
    /// copy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_key: Option<String>,
}

/// This is the `status` field for [`CopyTargetCrd`].
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CopyTargetStatus {
    /// The session object of the original operator session that created this CopyTarget.
    pub creator_session: Session,
    /// Current phase of the copy.
    ///
    /// Not filled by older operator versions.
    pub phase: Option<CopyTargetPhase>,
    /// Optional message describing the reason for copy failure.
    ///
    /// Only set when `phase` is `Failed`.
    pub failure_message: Option<String>,
    /// When the copy becomes eligible for deletion.
    pub expires_at: Option<Time>,
}

/// Legacy form of [`CopyTargetStatus].
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct CopyTargetStatusLegacy {
    /// The session object of the original operator session that created this CopyTarget.
    pub creator_session: Session,
    /// Current phase of the copy.
    ///
    /// Not filled by older operator versions.
    pub phase: Option<CopyTargetPhase>,
    /// Optional message describing the reason for copy failure.
    ///
    /// Only set when `phase` is `Failed`.
    pub failure_message: Option<String>,
}

/// The shape a copy target's status is served in.
///
/// Current clients read [`CopyTargetStatus`]; clients released before it read
/// [`CopyTargetStatusLegacy`], which the operator serves them instead. Untagged, so either shape
/// deserializes: the two are unambiguous because one requires `creatorSession` and the other
/// `creator_session`.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum CopyTargetStatusCompat {
    Modern(CopyTargetStatus),
    Legacy(CopyTargetStatusLegacy),
}

impl JsonSchema for CopyTargetStatusCompat {
    fn schema_name() -> Cow<'static, str> {
        "CopyTargetStatus".into()
    }

    fn json_schema(schema_gen: &mut schemars::SchemaGenerator) -> schemars::Schema {
        CopyTargetStatus::json_schema(schema_gen)
    }
}

impl CopyTargetStatusCompat {
    pub fn creator_session(&self) -> &Session {
        match self {
            Self::Modern(status) => &status.creator_session,
            Self::Legacy(status) => &status.creator_session,
        }
    }

    pub fn phase(&self) -> Option<&CopyTargetPhase> {
        match self {
            Self::Modern(status) => status.phase.as_ref(),
            Self::Legacy(status) => status.phase.as_ref(),
        }
    }

    pub fn failure_message(&self) -> Option<&str> {
        match self {
            Self::Modern(status) => status.failure_message.as_deref(),
            Self::Legacy(status) => status.failure_message.as_deref(),
        }
    }

    /// Absent in the legacy shape, which has no field for it.
    pub fn expires_at(&self) -> Option<&Time> {
        match self {
            Self::Modern(status) => status.expires_at.as_ref(),
            Self::Legacy(_) => None,
        }
    }

    /// Rewrites this status into the shape a client released before `expiresAt` reads.
    #[must_use]
    pub fn into_legacy(self) -> Self {
        match self {
            Self::Modern(status) => Self::Legacy(CopyTargetStatusLegacy {
                creator_session: status.creator_session,
                phase: status.phase,
                failure_message: status.failure_message,
            }),
            legacy => legacy,
        }
    }
}

/// Stage a copied pod has reached.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub enum CopyTargetPhase {
    InProgress,
    Ready,
    Failed,
    #[serde(untagged)]
    Unknown(String),
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

#[cfg(test)]
mod status_compat {
    use super::*;

    fn session() -> serde_json::Value {
        serde_json::json!({"duration_secs": 1, "user": "u", "target": "t"})
    }

    fn modern() -> CopyTargetStatusCompat {
        serde_json::from_value(serde_json::json!({
            "creatorSession": session(),
            "phase": "Ready",
            "expiresAt": "2026-01-01T00:00:00Z",
        }))
        .expect("the modern shape deserializes")
    }

    #[test]
    fn a_current_client_is_served_the_expiry() {
        let value = serde_json::to_value(modern()).expect("serializes");

        assert!(value.get("creatorSession").is_some(), "{value}");
        assert!(value.get("expiresAt").is_some(), "{value}");
    }

    #[test]
    fn a_legacy_client_is_served_neither() {
        let value = serde_json::to_value(modern().into_legacy()).expect("serializes");

        assert!(value.get("creator_session").is_some(), "{value}");
        assert!(value.get("creatorSession").is_none(), "{value}");
        assert!(value.get("expiresAt").is_none(), "{value}");
    }

    #[test]
    fn each_shape_deserializes_to_its_own_variant() {
        let legacy: CopyTargetStatusCompat =
            serde_json::from_value(serde_json::json!({"creator_session": session()}))
                .expect("the legacy shape deserializes");

        assert!(matches!(legacy, CopyTargetStatusCompat::Legacy(..)));
        assert!(matches!(modern(), CopyTargetStatusCompat::Modern(..)));
        assert_eq!(legacy.expires_at(), None);
    }
}
