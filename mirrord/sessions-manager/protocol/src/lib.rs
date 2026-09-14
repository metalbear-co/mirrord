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
//! This crate only describes sessions-manager coordination and connection
//! establishment. The mirrord protocol carried over the resulting connection is
//! defined by `mirrord-protocol`.

pub use control_plane::{
    AgentInstanceId, AssignmentId, AssignmentRole, AssignmentSubscription, ControlPlaneEventName,
    IntproxyConnectionId,
};
pub use data_plane::{DataPlaneAuthorization, DataPlaneEndpoint};
pub use error::SessionsManagerProtocolError;
use serde::{Deserialize, Serialize};

mod control_plane;
mod data_plane;
mod error;

/// Information a control-plane subscription sends to one peer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionAssignment {
    pub assignment_id: AssignmentId,
    pub data_plane_endpoint: DataPlaneEndpoint,
    pub authorization: DataPlaneAuthorization,
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
        };

        assert!(!format!("{assignment:?}").contains("secret"));
    }
}
