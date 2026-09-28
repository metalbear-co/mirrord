use serde::{Deserialize, Serialize};
use strum_macros::{AsRefStr, EnumString};
use uuid::Uuid;

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
    /// Carries a [`crate::ConnectionAssignment`] as its JSON body.
    Assignment,
    /// Carries an empty body; signals that a newer registration replaced this subscription.
    Superseded,
}

/// Query parameters used to attach to an assignment SSE stream.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum AssignmentSubscription {
    Agent(AgentIdentity),
    Intproxy {
        #[serde(flatten)]
        identity: IntproxyIdentity,
        /// Restricts pairing to the agent on this replica. This is a routing constraint, not part
        /// of the connection's identity.
        #[serde(skip_serializing_if = "Option::is_none")]
        agent_replica_filter: Option<ReplicaId>,
    },
}

/// Identifies a service in the sessions-manager control plane.
///
/// This is the service identity of the remote workload: every agent registration and intproxy
/// connection is scoped to one `(environment, service)`, and pairing only ever happens between
/// peers that share it. The pair selects a scope for routing; it is not an authorization identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceScope {
    /// Environment containing the service: the deployment context (e.g. `staging`, `dev`) that
    /// the workload and its agents run in, outside Kubernetes. It namespaces `service`, so the
    /// same service name in different environments is a different scope. Appears as
    /// `/v1/env/{environment}/...` in control-plane URLs.
    pub environment: String,
    /// Service within the environment: the logical workload whose replicas each run an agent
    /// companion. All replicas of the workload share it; individual replicas are told apart by
    /// [`ReplicaId`]. Appears as `/v1/env/{environment}/service/{service}/...` in control-plane
    /// URLs.
    pub service: String,
}

/// Full identity of one agent registration against sessions-manager.
///
/// `replica_id` says *which replica* the agent serves; `agent_instance_id` says *which run* of
/// the agent on that replica. Only the pair identifies a registration.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentIdentity {
    /// The workload replica (e.g. one ECS task) this agent is the companion of. Survives agent
    /// restarts.
    pub replica_id: ReplicaId,
    /// v4 UUID generated when the agent starts. Distinguishes successive agents that share a
    /// `replica_id` (container restart, image replacement), so a stale registration's cleanup
    /// cannot take a newer one offline. Kept across SSE reconnects of the same process.
    pub agent_instance_id: Uuid,
}

impl AgentIdentity {
    /// Identity for a freshly started agent on `replica_id`, with a new `agent_instance_id`.
    pub fn new(replica_id: ReplicaId) -> Self {
        Self {
            replica_id,
            agent_instance_id: Uuid::new_v4(),
        }
    }
}

/// Full identity of one intproxy connection against sessions-manager.
///
/// `user_session_id` groups connections that belong to the same developer session;
/// `intproxy_connection_id` distinguishes the connections within it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct IntproxyIdentity {
    /// The developer's logical mirrord session, used for ownership and analytics. Shared by all
    /// of its intproxy connections.
    pub user_session_id: String,
    /// One independently paired and allocated connection within `user_session_id`. Kept across
    /// SSE reconnects; a separately established connection gets a new one, so connections in the
    /// same session never replace or share credentials with each other.
    pub intproxy_connection_id: String,
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
    ///
    /// An assignment is the pairing of exactly one intproxy connection with exactly one agent
    /// (replica) endpoint, backed by its own data plane. Both peers receive an `assignment` event
    /// carrying the same id, so it names the shared allocation from either side. It is not a
    /// credential: the single-use, role-specific `authorization` delivered alongside it is what
    /// admits a peer to the data plane.
    AssignmentId
);
string_id!(
    /// Identifies one workload replica (e.g. one ECS task) within a service.
    ///
    /// Stable across agent restarts on that replica, which is why an intproxy can target it with
    /// `agent_replica_filter`. Agents take it from `MIRRORD_REMOTE_SERVICE_REPLICA`, defaulting to
    /// the id from the hosting platform's metadata (e.g. the ECS task ARN) and then the hostname.
    /// It must be unique among concurrently running replicas of a service; nothing verifies this.
    ReplicaId
);
