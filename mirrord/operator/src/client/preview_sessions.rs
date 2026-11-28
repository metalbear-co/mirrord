//! The client the CLI manages `PreviewSession`s with.
//!
//! An operator that advertises `PreviewSessionRoutes` serves the sessions under its own API group
//! (see [`PREVIEW_SESSIONS_ROUTE_GROUP_VERSION`]). The CLI keeps its typed `Api<PreviewSession>`
//! calls (list, get, create, delete, watch) and hands them a client whose requests for the
//! resource go to that route instead: [`routed_through_operator`] rewrites only the path, so the
//! query string, the body, the watch stream and every response stay what the Kubernetes API
//! would send.

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use http::{Request, Response, Uri, uri::PathAndQuery};
use kube::{Client, client::Body};
use tower::Service;

use crate::{
    client::error::{OperatorApiError, OperatorApiResult},
    crd::{
        MirrordOperatorSpec, NewOperatorFeature,
        preview::{PREVIEW_SESSIONS_ROUTE_GROUP_VERSION, preview_sessions_route_path},
    },
};

/// Where the CLI sends its `PreviewSession` requests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionPath {
    /// The operator's preview session route, see [`routed_through_operator`].
    OperatorRoute,
    /// The resource itself, straight through the Kubernetes API, as CLIs did before the route.
    KubernetesApi,
}

/// Picks the [`SessionPath`] for an operator with `spec`: its route whenever it advertises
/// `PreviewSessionRoutes`, and otherwise the Kubernetes API, unless the CLI is logged in to it
/// through SSO, which reaches nothing but the operator's API. That case is refused with the
/// error naming the operator version that lacks the route.
pub fn session_path(
    spec: &MirrordOperatorSpec,
    through_login: bool,
) -> OperatorApiResult<SessionPath> {
    match spec.require_feature(NewOperatorFeature::PreviewSessionRoutes) {
        Ok(()) => Ok(SessionPath::OperatorRoute),
        Err(
            OperatorApiError::UnsupportedFeature { .. } | OperatorApiError::FeatureDisabled { .. },
        ) if !through_login => Ok(SessionPath::KubernetesApi),
        Err(error) => Err(error),
    }
}

/// Wraps `client` so that its `PreviewSession` requests reach the operator's preview session
/// route. Requests for anything else go out unchanged.
///
/// `client` keeps doing the work (its base URL, its credentials, its timeouts), so this works
/// the same with a kubeconfig, where the API server forwards the route to the operator, and with
/// an SSO login, where `client` already talks to the operator.
pub fn routed_through_operator(client: &Client) -> Client {
    Client::new(
        RouteRewrite {
            inner: client.clone(),
        },
        client.default_namespace(),
    )
}

/// The service behind [`routed_through_operator`]: sends each request through `inner`, with the
/// path of a `PreviewSession` request moved to the route.
#[derive(Clone)]
struct RouteRewrite {
    inner: Client,
}

impl Service<Request<Body>> for RouteRewrite {
    type Response = Response<Body>;
    type Error = kube::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Response<Body>, kube::Error>> + Send>>;

    /// `inner` waits for its own readiness inside [`Client::send`].
    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<Body>) -> Self::Future {
        let inner = self.inner.clone();
        Box::pin(async move { inner.send(with_route_path(request)).await })
    }
}

/// `request` with its path moved to the preview session route when it names the
/// `PreviewSession` resource.
fn with_route_path(mut request: Request<Body>) -> Request<Body> {
    let Some(path) = preview_sessions_route_path(request.uri().path()) else {
        return request;
    };
    let path_and_query = match request.uri().query() {
        Some(query) => format!("{path}?{query}"),
        None => path,
    };
    // A path built from a valid one with a fixed prefix swapped is valid too, so this only
    // falls back to the original request if that ever stops holding.
    let Ok(path_and_query) = path_and_query.parse::<PathAndQuery>() else {
        tracing::warn!(
            uri = %request.uri(),
            "could not rewrite a PreviewSession request to {PREVIEW_SESSIONS_ROUTE_GROUP_VERSION}"
        );
        return request;
    };
    let mut parts = request.uri().clone().into_parts();
    parts.path_and_query = Some(path_and_query);
    if let Ok(uri) = Uri::from_parts(parts) {
        *request.uri_mut() = uri;
    }
    request
}

#[cfg(test)]
mod tests {
    use kube::Resource;

    use super::*;
    use crate::crd::preview::PreviewSession;

    fn resource_path(rest: &str) -> String {
        format!(
            "/apis/{}/{}/{rest}",
            PreviewSession::group(&()),
            PreviewSession::version(&())
        )
    }

    fn rewritten(uri: &str) -> String {
        let request = Request::builder()
            .uri(uri)
            .body(Body::empty())
            .expect("a test request builds");
        with_route_path(request).uri().to_string()
    }

    /// Every request the CLI's typed API sends for a session moves to the route, with the query
    /// (label and field selectors, watch parameters) kept as it was.
    #[test]
    fn session_requests_move_to_the_route_with_their_query() {
        assert_eq!(
            rewritten(&resource_path(
                "namespaces/apps/previewsessions?labelSelector=a%3Db&watch=true"
            )),
            "/apis/operator.metalbear.co/v1alpha1/namespaces/apps/previewsessions?labelSelector=a%3Db&watch=true"
        );
        assert_eq!(
            rewritten(&resource_path("namespaces/apps/previewsessions/pr-1")),
            "/apis/operator.metalbear.co/v1alpha1/namespaces/apps/previewsessions/pr-1"
        );
        assert_eq!(
            rewritten(&resource_path("previewsessions")),
            "/apis/operator.metalbear.co/v1alpha1/previewsessions"
        );
    }

    fn operator(features: &[NewOperatorFeature]) -> MirrordOperatorSpec {
        serde_json::from_value(serde_json::json!({
            "operator_version": "3.240.0",
            "default_namespace": "default",
            "supported_features": features,
            "license": {"name": "test", "organization": "test", "expire_at": "2099-01-01"}
        }))
        .expect("a test operator spec parses")
    }

    /// An operator with the route gets every session request, whatever the login, as
    /// `operator-apiservice.mdc` asks of mirrord's resources.
    #[test]
    fn an_operator_with_the_route_gets_the_requests() {
        let spec = operator(&[NewOperatorFeature::PreviewSessionRoutes]);
        for through_login in [false, true] {
            assert_eq!(
                session_path(&spec, through_login).unwrap(),
                SessionPath::OperatorRoute
            );
        }
    }

    /// An older operator keeps working with a kubeconfig, through the resource itself. Logged in
    /// through SSO, that resource is out of reach, so the CLI names the missing feature instead
    /// of failing on a 404 from the operator's URL.
    #[test]
    fn an_older_operator_works_only_with_a_kubeconfig() {
        let spec = operator(&[NewOperatorFeature::PreviewEnv]);
        assert_eq!(
            session_path(&spec, false).unwrap(),
            SessionPath::KubernetesApi
        );
        assert!(matches!(
            session_path(&spec, true),
            Err(OperatorApiError::UnsupportedFeature {
                feature: NewOperatorFeature::PreviewSessionRoutes,
                ..
            })
        ));
    }

    /// Requests for anything else, the operator's own routes included, go out as they came.
    #[test]
    fn other_requests_are_left_alone() {
        for uri in [
            "/apis/operator.metalbear.co/v1alpha1/namespaces/apps/previews/pr-1/logs",
            "/api/v1/namespaces/apps/pods",
            "/apis/apps/v1/namespaces/apps/deployments/previewsessions",
        ] {
            assert_eq!(rewritten(uri), uri);
        }
    }
}
