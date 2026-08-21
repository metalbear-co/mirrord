use std::fmt;

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
pub struct CopyTargetStatus {
    /// The session object of the original operator session that created this CopyTarget.
    pub creator_session: Session,
    /// Current phase of the copy.
    ///
    /// Absent on copies created by operator versions from before the field existed; treat that as
    /// [`CopyTargetPhase::Ready`], which is what those versions meant by reaching this point.
    pub phase: Option<CopyTargetPhase>,
    /// Optional message describing the reason for copy failure.
    ///
    /// Only set when `phase` is `Failed`.
    pub failure_message: Option<String>,
}

/// Stage a copied pod has reached.
///
/// [`Self::Unknown`] keeps the raw value of a phase this build does not know, so a newer operator
/// naming a stage this one has never heard of still deserializes, and the CLI can report what it
/// actually saw.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub enum CopyTargetPhase {
    InProgress,
    Ready,
    Failed,
    #[serde(untagged)]
    Unknown(String),
}

impl fmt::Display for CopyTargetPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InProgress => f.write_str("InProgress"),
            Self::Ready => f.write_str("Ready"),
            Self::Failed => f.write_str("Failed"),
            Self::Unknown(phase) => f.write_str(phase),
        }
    }
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
mod copy_target_phase_wire_format {
    use super::*;

    /// The phase was a bare string before it was an enum, and both the CLI and released operators
    /// exchange it by value, so the strings may not drift.
    #[test]
    fn known_phases_keep_their_strings() {
        for (phase, expected) in [
            (CopyTargetPhase::InProgress, "InProgress"),
            (CopyTargetPhase::Ready, "Ready"),
            (CopyTargetPhase::Failed, "Failed"),
        ] {
            assert_eq!(serde_json::to_value(&phase).unwrap(), expected);
            assert_eq!(
                serde_json::from_value::<CopyTargetPhase>(serde_json::json!(expected)).unwrap(),
                phase
            );
        }
    }

    /// A phase this build does not know keeps its value rather than collapsing, so the CLI can name
    /// it and a newer operator's object still deserializes.
    #[test]
    fn an_unknown_phase_keeps_its_value() {
        let parsed: CopyTargetPhase =
            serde_json::from_value(serde_json::json!("Draining")).unwrap();

        assert_eq!(parsed, CopyTargetPhase::Unknown("Draining".to_owned()));
        assert_eq!(parsed.to_string(), "Draining");
        assert_eq!(serde_json::to_value(&parsed).unwrap(), "Draining");
    }
}
