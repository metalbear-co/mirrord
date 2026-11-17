//! The listing of live sessions a standalone sessions-manager serves at `GET /v1/sessions`.
//!
//! Without an operator there is no cluster resource to read sessions from, so `mirrord ui` asks
//! the sessions-manager the developer is logged in to, the same way it reads the operator's status
//! when one is installed. The caller sees every session of the organization their `mirrord login`
//! token names, which matches what the operator shows its users.

use serde::{Deserialize, Serialize};

use crate::{ReplicaId, ServiceScope};

/// Every live session of the caller's organization.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionList {
    pub sessions: Vec<SessionSummary>,
}

/// One intproxy connection paired with an agent. A mirrord run that connects several times shows
/// up once per connection, all sharing its `user_session_id`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummary {
    /// Unique among the listed sessions, and stable for as long as the pairing lasts.
    pub id: String,
    pub scope: ServiceScope,
    /// The replica whose agent serves the session.
    pub agent_replica_id: ReplicaId,
    /// The developer's mirrord run, shared by all of its connections.
    pub user_session_id: String,
    /// Absent when the sessions-manager does not authenticate developers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<SessionOwner>,
    /// When the pairing was made, in seconds since the Unix epoch.
    pub started_at: u64,
    /// Whether both sides have attached to the data plane. A pairing whose sides never attach is
    /// dropped after the sessions-manager's attach timeout.
    pub connected: bool,
}

/// The developer who started a session, as their `mirrord login` token names them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionOwner {
    pub user_id: String,
    pub email: String,
}
