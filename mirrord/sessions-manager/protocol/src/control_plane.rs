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
        agent_instance_id: AgentInstanceId,
    },
    Intproxy {
        /// Stable user-session identity used for session ownership and analytics.
        user_session_id: String,
        /// Identifies one independently assigned intproxy connection within `user_session_id`.
        intproxy_connection_id: IntproxyConnectionId,
        #[serde(skip_serializing_if = "Option::is_none")]
        agent_replica_filter: Option<String>,
    },
}

/// Defines a newtype wrapper over `String` with the `new`/`as_str`/`From<String>`/`Display` impls
/// every plain string-identity type in this protocol needs, so adding one is a single line instead
/// of repeating that boilerplate.
macro_rules! string_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

string_id!(
    /// Identifies one assignment across control-plane reconnects.
    AssignmentId
);
string_id!(
    /// Identifies one logical agent registration across SSE reconnects.
    AgentInstanceId
);
string_id!(
    /// Identifies one independently assigned intproxy connection across its SSE reconnects.
    IntproxyConnectionId
);
