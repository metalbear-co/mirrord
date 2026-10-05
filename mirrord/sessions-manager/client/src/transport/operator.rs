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

/// Bound on receiving response headers for the assignments subscription through kube-apiserver.
///
/// Covers only the initial response; the event stream body stays open without a deadline.
const RESPONSE_HEADER_TIMEOUT: Duration = Duration::from_secs(30);

/// Bound on the whole data-plane dial through kube-apiserver, including the WebSocket upgrade.
const WEBSOCKET_UPGRADE_TIMEOUT: Duration = Duration::from_secs(30);

const OPERATOR_DATA_PLANES_PATH: &str =
    "/apis/operator.metalbear.co/v1alpha1/serverlessdataplanes/";

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

    /// Upgrades through kube-apiserver to the operator's `serverlessdataplanes/{id}` route.
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

/// Builds the operator's per-role assignment request URI. The scope is slugified the same way
/// the standalone sessions-manager's path segments are, so both deployments register a scope
/// under the same key.
fn operator_assignments_uri(
    scope: &ServiceScope,
    subscription: &AssignmentSubscription,
) -> Result<String, SessionsManagerClientError> {
    let resource = match subscription {
        AssignmentSubscription::Agent(_) => "serverlessagentassignments",
        AssignmentSubscription::Intproxy { .. } => "serverlessclientassignments",
    };
    let environment = slug::slugify(&scope.environment);
    let service = slug::slugify(&scope.service);
    let subscription = serde_urlencoded::to_string(subscription)?;

    Ok(format!(
        "/apis/operator.metalbear.co/v1alpha1/proxy/{resource}/{environment}.{service}?{subscription}"
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
    let endpoint = assignment.data_plane_endpoint.as_str();
    let allocation_id = endpoint
        .strip_prefix(OPERATOR_DATA_PLANES_PATH)
        .and_then(|allocation_id| uuid::Uuid::parse_str(allocation_id).ok());
    if !allocation_id.is_some_and(|allocation_id| {
        endpoint == format!("{OPERATOR_DATA_PLANES_PATH}{}", allocation_id.hyphenated())
    }) {
        return Err(SessionsManagerClientError::InvalidOperatorDataPlaneEndpoint);
    }

    Ok(Request::get(assignment.data_plane_endpoint.as_str())
        .header(
            OPERATOR_DATA_PLANE_AUTHORIZATION_HEADER,
            assignment_authorization(assignment)?,
        )
        .body(Vec::new())?)
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use futures::{SinkExt, StreamExt};
    use hyper::{
        Response,
        header::{AUTHORIZATION, CONTENT_TYPE},
    };
    use mirrord_sessions_manager_protocol::{AgentIdentity, IntproxyIdentity};
    use tokio_tungstenite::{
        WebSocketStream,
        tungstenite::{
            Message,
            handshake::derive_accept_key,
            protocol::{CloseFrame, Role, frame::coding::CloseCode},
        },
    };
    use uuid::Uuid;

    use super::*;

    fn assignment(endpoint: &str) -> ConnectionAssignment {
        serde_json::from_value(serde_json::json!({
            "assignment_id": "assignment-1",
            "data_plane_endpoint": endpoint,
            "authorization": "Bearer secret",
        }))
        .unwrap()
    }

    fn mock_transport(
        status: StatusCode,
        content_type: &'static str,
        body: &'static str,
    ) -> OperatorTransport {
        let service = tower::service_fn(move |request: Request<kube::client::Body>| async move {
            assert_eq!(request.headers()[ACCEPT], "text/event-stream");
            assert_eq!(
                request.uri(),
                "/apis/operator.metalbear.co/v1alpha1/proxy/serverlessagentassignments/staging-eu.payments?role=agent&replica_id=pod-a&agent_instance_id=00000000-0000-0000-0000-000000000000"
            );
            Ok::<_, Infallible>(
                Response::builder()
                    .status(status)
                    .header(CONTENT_TYPE, content_type)
                    .body(kube::client::Body::from(body.as_bytes().to_vec()))
                    .unwrap(),
            )
        });
        OperatorTransport::new(kube::Client::new(service, "default"))
    }

    async fn subscribe(
        transport: &OperatorTransport,
    ) -> Result<ControlPlaneEventStream, SessionsManagerClientError> {
        transport
            .subscribe_assignments(
                &ServiceScope {
                    environment: "Staging EU".to_owned(),
                    service: "payments".to_owned(),
                },
                &AssignmentSubscription::Agent(AgentIdentity {
                    replica_id: "pod-a".to_owned().into(),
                    agent_instance_id: Uuid::nil(),
                }),
            )
            .await
    }

    #[tokio::test]
    async fn subscription_decodes_mocked_kube_response() {
        let transport = mock_transport(
            StatusCode::OK,
            "text/event-stream",
            concat!(
                ": keep-alive\n\n",
                "event: assignment\ndata: {\"assignment_id\":\"assignment-1\",",
                "\"data_plane_endpoint\":\"/apis/operator.metalbear.co/v1alpha1/serverlessdataplanes/00000000-0000-0000-0000-000000000000\",",
                "\"authorization\":\"Bearer secret\"}\n\n",
                "event: superseded\ndata: {}\n\n"
            ),
        );
        let mut stream = subscribe(&transport).await.unwrap();
        assert!(matches!(
            stream.next().await.unwrap(),
            Some(Ok(crate::control_plane::ControlPlaneEvent::Assignment(_)))
        ));
        assert!(matches!(
            stream.next().await.unwrap(),
            Some(Ok(crate::control_plane::ControlPlaneEvent::Superseded))
        ));
        assert!(stream.next().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn subscription_classifies_mocked_kube_statuses() {
        for status in [
            StatusCode::NOT_FOUND,
            StatusCode::NOT_IMPLEMENTED,
            StatusCode::UNAUTHORIZED,
            StatusCode::FORBIDDEN,
            StatusCode::BAD_REQUEST,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::SERVICE_UNAVAILABLE,
        ] {
            let transport = mock_transport(status, "application/json", "{}");
            let error = subscribe(&transport).await.err().unwrap();
            assert_eq!(
                error.is_retryable(),
                matches!(
                    status,
                    StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE
                )
            );
            assert_eq!(
                matches!(
                    error,
                    SessionsManagerClientError::ServerlessSessionsManagerNotServed(_)
                ),
                matches!(status, StatusCode::NOT_FOUND | StatusCode::NOT_IMPLEMENTED)
            );
        }
    }

    #[tokio::test]
    async fn subscription_rejects_non_sse_response() {
        let transport = mock_transport(StatusCode::OK, "application/json", "{}");
        assert!(matches!(
            subscribe(&transport).await,
            Err(SessionsManagerClientError::InvalidContentType(_))
        ));
    }

    #[tokio::test]
    async fn data_plane_kube_status_errors_are_terminal() {
        for status in [StatusCode::UNAUTHORIZED, StatusCode::NOT_FOUND] {
            let service = tower::service_fn(
                move |request: Request<kube::client::Body>| async move {
                    assert_eq!(
                        request.headers()[OPERATOR_DATA_PLANE_AUTHORIZATION_HEADER],
                        "Bearer secret"
                    );
                    assert_eq!(
                        request.headers()[hyper::header::SEC_WEBSOCKET_PROTOCOL],
                        "v4.channel.k8s.io"
                    );
                    let body = serde_json::json!({"kind": "Status", "apiVersion": "v1", "status": "Failure", "message": "rejected", "code": status.as_u16()});
                    Ok::<_, Infallible>(
                        Response::builder()
                            .status(status)
                            .body(kube::client::Body::from(serde_json::to_vec(&body).unwrap()))
                            .unwrap(),
                    )
                },
            );
            let transport = OperatorTransport::new(kube::Client::new(service, "default"));
            let error = transport.connect_data_plane::<mirrord_protocol_io::Client>(assignment("/apis/operator.metalbear.co/v1alpha1/serverlessdataplanes/00000000-0000-0000-0000-000000000000")).await.err().unwrap();
            assert!(
                matches!(&error, SessionsManagerClientError::Kube(error) if matches!(error.as_ref(), kube::Error::Api(error) if error.code == status.as_u16()))
            );
            assert!(!error.is_retryable());
        }
    }

    async fn websocket_transport(
        subprotocol: &'static str,
    ) -> (OperatorTransport, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let service = hyper::service::service_fn(
                move |mut request: Request<hyper::body::Incoming>| async move {
                    assert_eq!(
                        request.headers()[OPERATOR_DATA_PLANE_AUTHORIZATION_HEADER],
                        "Bearer secret"
                    );
                    assert_eq!(
                        request.headers()[hyper::header::SEC_WEBSOCKET_PROTOCOL],
                        "v4.channel.k8s.io"
                    );
                    let accept = derive_accept_key(
                        request.headers()[hyper::header::SEC_WEBSOCKET_KEY].as_bytes(),
                    );
                    let upgrade = hyper::upgrade::on(&mut request);
                    tokio::spawn(async move {
                        if let Ok(upgraded) = upgrade.await {
                            let mut socket = WebSocketStream::from_raw_socket(
                                hyper_util::rt::TokioIo::new(upgraded),
                                Role::Server,
                                None,
                            )
                            .await;
                            let _ = socket
                                .send(Message::Close(Some(CloseFrame {
                                    code: CloseCode::Policy,
                                    reason: "invalid assignment authorization".into(),
                                })))
                                .await;
                        }
                    });
                    Ok::<_, Infallible>(
                        Response::builder()
                            .status(StatusCode::SWITCHING_PROTOCOLS)
                            .header(hyper::header::UPGRADE, "websocket")
                            .header(hyper::header::CONNECTION, "Upgrade")
                            .header(hyper::header::SEC_WEBSOCKET_ACCEPT, accept)
                            .header(hyper::header::SEC_WEBSOCKET_PROTOCOL, subprotocol)
                            .body(http_body_util::Empty::<bytes::Bytes>::new())
                            .unwrap(),
                    )
                },
            );
            hyper::server::conn::http1::Builder::new()
                .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
                .with_upgrades()
                .await
                .unwrap();
        });
        let config = kube::Config::new(format!("http://{address}").parse().unwrap());
        (
            OperatorTransport::new(kube::Client::try_from(config).unwrap()),
            server,
        )
    }

    #[tokio::test]
    async fn data_plane_upgrades_and_surfaces_policy_close() {
        let (transport, server) = websocket_transport("v4.channel.k8s.io").await;
        let mut connection = transport.connect_data_plane::<mirrord_protocol_io::Client>(assignment("/apis/operator.metalbear.co/v1alpha1/serverlessdataplanes/00000000-0000-0000-0000-000000000000")).await.unwrap();
        let error = tokio::time::timeout(Duration::from_secs(5), connection.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(
            matches!(error, mirrord_operator_websocket::connection::OperatorConnectionError::InvalidMessage(message) if matches!(message.as_ref(), Message::Close(Some(frame)) if frame.code == CloseCode::Policy))
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn data_plane_rejects_incorrect_subprotocol() {
        let (transport, server) = websocket_transport("incorrect").await;
        let error = transport.connect_data_plane::<mirrord_protocol_io::Client>(assignment("/apis/operator.metalbear.co/v1alpha1/serverlessdataplanes/00000000-0000-0000-0000-000000000000")).await.err().unwrap();
        assert!(
            matches!(&error, SessionsManagerClientError::Kube(error) if matches!(error.as_ref(), kube::Error::UpgradeConnection(kube::client::UpgradeConnectionError::SecWebSocketProtocolMismatch)))
        );
        assert!(!error.is_retryable());
        server.await.unwrap();
    }

    #[test]
    fn data_plane_request_rejects_noncanonical_operator_routes() {
        for endpoint in [
            "/ws/00000000-0000-0000-0000-000000000000",
            "/apis/operator.metalbear.co/v1alpha1/serverlessdataplanes/",
            "/apis/operator.metalbear.co/v1alpha1/serverlessdataplanes/assignment-1",
            "/apis/operator.metalbear.co/v1alpha1/serverlessdataplanes/00000000000000000000000000000000",
            "/apis/operator.metalbear.co/v1alpha1/serverlessdataplanes/00000000-0000-0000-0000-000000000000/extra",
            "/apis/operator.metalbear.co/v1alpha1/serverlessdataplanes/00000000-0000-0000-0000-000000000000?peer=agent",
            "/apis/operator.metalbear.co/v1alpha1/serverlessdataplanes/%30%30%30%30%30%30%30%30-0000-0000-0000-000000000000",
        ] {
            let error = data_plane_request(&assignment(endpoint)).unwrap_err();
            assert!(matches!(
                error,
                SessionsManagerClientError::InvalidOperatorDataPlaneEndpoint
            ));
            assert!(!error.is_retryable());
        }
    }

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
            agent_replica_filter: Some("pod-a".to_owned().into()),
        };

        assert_eq!(
            operator_assignments_uri(&scope, &subscription).unwrap(),
            "/apis/operator.metalbear.co/v1alpha1/proxy/serverlessclientassignments/staging-eu.payments\
             ?role=intproxy&user_session_id=session-a&intproxy_connection_id=connection-a\
             &agent_replica_filter=pod-a"
        );
    }

    #[test]
    fn operator_assignments_uri_selects_agent_resource() {
        let scope = ServiceScope {
            environment: "Staging EU".to_owned(),
            service: "API #1".to_owned(),
        };
        let subscription = AssignmentSubscription::Agent(AgentIdentity {
            replica_id: "pod-a".to_owned().into(),
            agent_instance_id: Uuid::nil(),
        });

        assert_eq!(
            operator_assignments_uri(&scope, &subscription).unwrap(),
            "/apis/operator.metalbear.co/v1alpha1/proxy/serverlessagentassignments/staging-eu.api-1\
             ?role=agent&replica_id=pod-a&agent_instance_id=00000000-0000-0000-0000-000000000000"
        );
    }

    /// kube-apiserver consumes `Authorization` for the caller's own identity, so the assignment
    /// token has to travel in a header it forwards to the operator.
    #[test]
    fn data_plane_request_carries_assignment_token_outside_authorization() {
        let assignment: ConnectionAssignment = serde_json::from_value(serde_json::json!({
            "assignment_id": "assignment-1",
            "data_plane_endpoint":
                "/apis/operator.metalbear.co/v1alpha1/serverlessdataplanes/00000000-0000-0000-0000-000000000000",
            "authorization": "Bearer secret",
        }))
        .unwrap();

        let request = data_plane_request(&assignment).unwrap();

        assert_eq!(
            request.uri(),
            "/apis/operator.metalbear.co/v1alpha1/serverlessdataplanes/00000000-0000-0000-0000-000000000000"
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
