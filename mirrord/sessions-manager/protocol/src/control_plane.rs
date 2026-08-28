use serde::{Deserialize, Serialize};
use strum_macros::{AsRefStr, EnumString};

/// Identifies the recipient of a control-plane assignment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssignmentRole {
    Agent,
    Intproxy,
}

/// Names of the SSE events sessions-manager sends over an assignment subscription.
///
/// The server names each `sse::Event` by one of these and the client matches the incoming SSE
/// event's name against the same set, so this lives here instead of being duplicated as string
/// literals in the server and client repos.
#[derive(Clone, Copy, Debug, PartialEq, Eq, AsRefStr, EnumString)]
#[strum(serialize_all = "snake_case")]
pub enum ControlPlaneEventName {
    /// Carries a [`ConnectionAssignment`] as its JSON body.
    Assignment,
    /// Carries an empty body; signals that a newer registration replaced this subscription.
    Superseded,
}

/// Query parameters used to attach to an assignment SSE stream.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case", deny_unknown_fields)]
pub enum AssignmentSubscription {
    Agent {
        replica_id: String,
        instance_id: AgentInstanceId,
    },
    Intproxy {
        session_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        target_replica_id: Option<String>,
    },
}

/// Identifies one assignment across control-plane reconnects.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AssignmentId(String);

impl AssignmentId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for AssignmentId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl std::fmt::Display for AssignmentId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Identifies one logical agent registration across SSE reconnects.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentInstanceId(String);

impl AgentInstanceId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for AgentInstanceId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl std::fmt::Display for AgentInstanceId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}
