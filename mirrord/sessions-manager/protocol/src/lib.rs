//! Types shared by the sessions-manager and its clients.
//!
//! This crate defines the serialized protocol used to coordinate connections
//! between mirrord agents and intproxies through the sessions-manager.
//!
//! Peers first subscribe to the sessions-manager control plane. Once matching
//! peers are found, the sessions-manager sends each peer a
//! [`ConnectionAssignment`] containing the data-plane endpoint and authorization
//! needed to establish the connection.
//!
//! A registration can say more than its identity in [`SUBSCRIPTION_OPTIONS_HEADER`]: a
//! developer can select agents by the tags of their tasks, or watch every matching task, and an
//! agent can name the task it serves and hold it for an admission verdict.
//!
//! This crate only describes sessions-manager coordination and connection
//! establishment. The mirrord protocol carried over the resulting connection is
//! defined by `mirrord-protocol`.

pub use control_plane::{
    Admission, AdmissionVerdict, AgentIdentity, AssignmentId, AssignmentRole,
    AssignmentSubscription, ControlPlaneEventName, IntproxyIdentity, ReplicaId, ServiceScope,
    TaskEnded, TaskMatch,
};
pub use data_plane::{
    DataPlaneAuthorization, DataPlaneEndpoint, OPERATOR_DATA_PLANE_AUTHORIZATION_HEADER,
};
pub use error::SessionsManagerProtocolError;
use serde::{Deserialize, Serialize};
pub use sessions::{SessionList, SessionOwner, SessionSummary};
pub use subscription_options::{
    AgentSelector, AgentSubscriptionOptions, AwsBatchJob, GateState, IntproxySubscriptionOptions,
    InvalidSubscriptionOptions, SUBSCRIPTION_OPTIONS_HEADER, SubscriptionOptions, Watch,
};

mod control_plane;
mod data_plane;
mod error;
mod sessions;
mod subscription_options;

/// Information a control-plane subscription sends to one peer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionAssignment {
    pub assignment_id: AssignmentId,
    pub data_plane_endpoint: DataPlaneEndpoint,
    pub authorization: DataPlaneAuthorization,
    /// The replica of the agent this assignment pairs with. Set only in the intproxy's
    /// assignment, by a sessions-manager that knows it.
    ///
    /// A session's first connection may be paired by selector
    /// ([`IntproxySubscriptionOptions::agent_selector`]), which picks among several matching
    /// tasks. The session's later connections register with `agent_replica_filter` set to this
    /// replica instead of the selector, so all of them reach the task the first one reached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_replica_id: Option<ReplicaId>,
}

#[cfg(test)]
mod tests {
    use http::Uri;

    use super::{ConnectionAssignment, DataPlaneAuthorization, DataPlaneEndpoint};

    #[test]
    fn serializes_http_assignment() {
        let assignment = ConnectionAssignment {
            assignment_id: "assignment-1".to_owned().into(),
            data_plane_endpoint: DataPlaneEndpoint::new(Uri::from_static("/sm/ws/123")).unwrap(),
            authorization: DataPlaneAuthorization::new("Bearer secret".to_owned()),
            agent_replica_id: None,
        };

        assert_eq!(
            serde_json::to_value(assignment).unwrap(),
            serde_json::json!({ "assignment_id": "assignment-1", "data_plane_endpoint": "/sm/ws/123", "authorization": "Bearer secret" })
        );
    }

    #[test]
    fn assignment_debug_redacts_authorization() {
        let assignment = ConnectionAssignment {
            assignment_id: "assignment-1".to_owned().into(),
            data_plane_endpoint: DataPlaneEndpoint::new(Uri::from_static("/sm/ws/123")).unwrap(),
            authorization: DataPlaneAuthorization::new("Bearer secret".to_owned()),
            agent_replica_id: None,
        };

        assert!(!format!("{assignment:?}").contains("secret"));
    }
}
