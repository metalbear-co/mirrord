pub use data_plane::{DataPlaneAuthorization, DataPlaneEndpoint};
pub use error::SessionsManagerProtocolError;
use serde::{Deserialize, Serialize};

mod data_plane;
mod error;

/// Information a control-plane subscription sends to one peer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionAssignment {
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
            data_plane_endpoint: DataPlaneEndpoint::new(Uri::from_static("/sm/ws/123")).unwrap(),
            authorization: DataPlaneAuthorization::new("Bearer secret".to_owned()),
        };

        assert_eq!(
            serde_json::to_value(assignment).unwrap(),
            serde_json::json!({ "data_plane_endpoint": "/sm/ws/123", "authorization": "Bearer secret" })
        );
    }

    #[test]
    fn assignment_debug_redacts_authorization() {
        let assignment = ConnectionAssignment {
            data_plane_endpoint: DataPlaneEndpoint::new(Uri::from_static("/sm/ws/123")).unwrap(),
            authorization: DataPlaneAuthorization::new("Bearer secret".to_owned()),
        };

        assert!(!format!("{assignment:?}").contains("secret"));
    }
}
