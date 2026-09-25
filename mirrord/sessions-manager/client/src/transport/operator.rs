//! The sessions-manager hosted by the mirrord operator, reached through kube-apiserver.

use std::time::Duration;

use http_body_util::BodyExt;
use hyper::{Request, StatusCode, header::ACCEPT};
use mirrord_operator_websocket::{connection::OperatorConnection, upgrade::connect_ws};
use mirrord_protocol_io::ProtocolEndpoint;
use mirrord_sessions_manager_protocol::{
    AssignmentSubscription, ConnectionAssignment, OPERATOR_DATA_PLANE_AUTHORIZATION_HEADER,
    ServiceScope,
};
use tokio::time::Instant;

use super::{SessionsManagerTransport, assignment_authorization};
use crate::{
    control_plane::{ControlPlaneEventStream, verify_event_stream},
    error::SessionsManagerClientError,
    retry::with_deadline,
};

/// Path of the operator's `sessionassignments` collection, served under the `proxy` verb so
/// kube-apiserver keeps the SSE stream open past its ordinary request timeout.
const OPERATOR_ASSIGNMENTS_PATH: &str =
    "/apis/operator.metalbear.co/v1alpha1/proxy/sessionassignments";

/// Bound on receiving response headers for the assignments subscription through kube-apiserver.
///
/// Covers only the initial response; the event stream body stays open without a deadline.
const RESPONSE_HEADER_TIMEOUT: Duration = Duration::from_secs(30);

/// Bound on the whole data-plane dial through kube-apiserver, including the WebSocket upgrade.
const WEBSOCKET_UPGRADE_TIMEOUT: Duration = Duration::from_secs(30);

/// Reaches the operator-hosted sessions-manager through kube-apiserver's aggregation of the
/// operator's APIService. Every request carries `client`'s own Kubernetes authentication.
#[derive(Clone)]
pub struct OperatorTransport {
    client: kube::Client,
}

impl OperatorTransport {
    pub fn new(client: kube::Client) -> Self {
        Self { client }
    }
}

impl SessionsManagerTransport for OperatorTransport {
    async fn subscribe_assignments(
        &self,
        scope: &ServiceScope,
        subscription: &AssignmentSubscription,
    ) -> Result<ControlPlaneEventStream, SessionsManagerClientError> {
        let deadline = Instant::now() + RESPONSE_HEADER_TIMEOUT;
        let uri = operator_assignments_uri(scope, subscription)?;
        tracing::debug!(
            %uri,
            "requesting operator-hosted sessions-manager assignments"
        );
        let request = Request::get(uri)
            .header(ACCEPT, "text/event-stream")
            .body(kube::client::Body::empty())?;
        let response = with_deadline(Some(deadline), self.client.send(request)).await??;
        if matches!(
            response.status(),
            StatusCode::NOT_FOUND | StatusCode::NOT_IMPLEMENTED
        ) {
            return Err(
                SessionsManagerClientError::ServerlessSessionsManagerNotServed(response.status()),
            );
        }
        verify_event_stream(response.status(), response.headers())?;

        Ok(ControlPlaneEventStream::from_bytes(
            response.into_body().into_data_stream(),
        ))
    }

    /// Upgrades through kube-apiserver to the operator's `sessiondataplanes/{id}` route.
    async fn connect_data_plane<E: ProtocolEndpoint + Send + Unpin + 'static>(
        &self,
        assignment: ConnectionAssignment,
    ) -> Result<OperatorConnection<E>, SessionsManagerClientError> {
        let request = data_plane_request(&assignment)?;
        let socket =
            tokio::time::timeout(WEBSOCKET_UPGRADE_TIMEOUT, connect_ws(&self.client, request))
                .await
                .map_err(|_| SessionsManagerClientError::WebSocketUpgradeTimeout)??;

        Ok(OperatorConnection::new(socket))
    }
}

/// Builds the operator's `sessionassignments` request URI. The scope is slugified the same way
/// the standalone sessions-manager's path segments are, so both deployments register a scope
/// under the same key.
fn operator_assignments_uri(
    scope: &ServiceScope,
    subscription: &AssignmentSubscription,
) -> Result<String, SessionsManagerClientError> {
    let scope = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("environment", &slug::slugify(&scope.environment))
        .append_pair("service", &slug::slugify(&scope.service))
        .finish();
    let subscription = serde_urlencoded::to_string(subscription)?;

    Ok(format!(
        "{OPERATOR_ASSIGNMENTS_PATH}?{scope}&{subscription}"
    ))
}

/// Builds the data-plane upgrade request for `assignment`.
///
/// The assignment endpoint is an absolute path that the Kubernetes client resolves against its
/// cluster URL, so the assignment authorization never leaves the cluster's API server. It travels
/// in [`OPERATOR_DATA_PLANE_AUTHORIZATION_HEADER`], leaving `Authorization` to the Kubernetes
/// client's own authentication.
fn data_plane_request(
    assignment: &ConnectionAssignment,
) -> Result<Request<Vec<u8>>, SessionsManagerClientError> {
    Ok(Request::get(assignment.data_plane_endpoint.as_str())
        .header(
            OPERATOR_DATA_PLANE_AUTHORIZATION_HEADER,
            assignment_authorization(assignment)?,
        )
        .body(Vec::new())?)
}

#[cfg(test)]
mod tests {
    use hyper::header::AUTHORIZATION;
    use mirrord_sessions_manager_protocol::IntproxyIdentity;

    use super::*;

    #[test]
    fn operator_assignments_uri_carries_slugified_scope_and_subscription() {
        let scope = ServiceScope {
            environment: "Staging EU".to_owned(),
            service: "payments".to_owned(),
        };
        let subscription = AssignmentSubscription::Intproxy {
            identity: IntproxyIdentity {
                user_session_id: "session-a".to_owned(),
                intproxy_connection_id: "connection-a".to_owned(),
            },
            agent_replica_filter: None,
        };

        assert_eq!(
            operator_assignments_uri(&scope, &subscription).unwrap(),
            "/apis/operator.metalbear.co/v1alpha1/proxy/sessionassignments\
             ?environment=staging-eu&service=payments\
             &role=intproxy&user_session_id=session-a&intproxy_connection_id=connection-a"
        );
    }

    /// kube-apiserver consumes `Authorization` for the caller's own identity, so the assignment
    /// token has to travel in a header it forwards to the operator.
    #[test]
    fn data_plane_request_carries_assignment_token_outside_authorization() {
        let assignment: ConnectionAssignment = serde_json::from_value(serde_json::json!({
            "assignment_id": "assignment-1",
            "data_plane_endpoint":
                "/apis/operator.metalbear.co/v1alpha1/sessiondataplanes/assignment-1",
            "authorization": "Bearer secret",
        }))
        .unwrap();

        let request = data_plane_request(&assignment).unwrap();

        assert_eq!(
            request.uri(),
            "/apis/operator.metalbear.co/v1alpha1/sessiondataplanes/assignment-1"
        );
        let token = request
            .headers()
            .get(OPERATOR_DATA_PLANE_AUTHORIZATION_HEADER)
            .unwrap();
        assert_eq!(token, "Bearer secret");
        assert!(token.is_sensitive());
        assert!(request.headers().get(AUTHORIZATION).is_none());
    }
}
