//! Sends a logged-in client's branch database requests to the operator's route for them.
//!
//! Under an SSO login the client reaches the operator at its public URL, which forwards only
//! `/apis/operator.metalbear.co/`. Branch databases are stored in `dbs.mirrord.metalbear.co`, so
//! requests for them would never arrive. The operator serves the same objects at
//! [`branch_databases_api_path`] and reads and writes them with its own account once the login's
//! RBAC check passed, so [`LoginRoutes`] only rewrites the path: the method, the query (`watch`,
//! `resourceVersion`, selectors) and the body stay as the branch code built them.

use std::task::{Context, Poll};

use http::{Request, Uri, uri::PathAndQuery};
use kube::Resource;
use tower::{Layer, Service};

use crate::crd::db_branching::branch_database::{BranchDatabase, branch_databases_api_path};

/// Adds [`LoginRoutes`] to a client. A client that reaches the operator through a kubeconfig gets
/// the layer too, switched off, so both kinds of client have one type.
#[derive(Clone, Copy, Debug)]
pub struct LoginRoutesLayer {
    through_login: bool,
}

impl LoginRoutesLayer {
    /// `through_login` is whether the client reaches the operator through an SSO login.
    pub fn new(through_login: bool) -> Self {
        Self { through_login }
    }
}

impl<S> Layer<S> for LoginRoutesLayer {
    type Service = LoginRoutes<S>;

    fn layer(&self, inner: S) -> Self::Service {
        LoginRoutes {
            inner,
            through_login: self.through_login,
        }
    }
}

/// See the module docs.
#[derive(Clone, Debug)]
pub struct LoginRoutes<S> {
    inner: S,
    through_login: bool,
}

impl<S, B> Service<Request<B>> for LoginRoutes<S>
where
    S: Service<Request<B>>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut request: Request<B>) -> Self::Future {
        if self.through_login
            && let Some(uri) = operator_route_uri(request.uri())
        {
            *request.uri_mut() = uri;
        }
        self.inner.call(request)
    }
}

/// `uri` with its path replaced by [`operator_route_path`], the query kept, or `None` when the
/// path is not a branch database path.
fn operator_route_uri(uri: &Uri) -> Option<Uri> {
    let path = operator_route_path(uri.path())?;
    let path_and_query = match uri.query() {
        Some(query) => format!("{path}?{query}"),
        None => path,
    };
    let mut parts = uri.clone().into_parts();
    parts.path_and_query = Some(PathAndQuery::try_from(path_and_query).ok()?);
    Uri::from_parts(parts).ok()
}

/// The operator route for a path of the stored [`BranchDatabase`] kind (collection, one object
/// or a subresource, in a namespace or across every namespace), `None` for any other path.
///
/// The stored group is resolved like the branch code resolves it, so a keyed copy of the operator
/// on staging and its CLI agree on it.
pub fn operator_route_path(path: &str) -> Option<String> {
    let stored_prefix = format!(
        "/apis/{}/{}/",
        BranchDatabase::group(&()),
        BranchDatabase::version(&())
    );
    let rest = path.strip_prefix(&stored_prefix)?;
    let plural = BranchDatabase::plural(&());

    let segments = rest.split('/').collect::<Vec<_>>();
    let (namespace, after_plural) = match segments.as_slice() {
        [resource, after @ ..] if *resource == plural => (None, after),
        ["namespaces", namespace, resource, after @ ..] if *resource == plural => {
            (Some(*namespace), after)
        }
        _ => return None,
    };

    let mut route = branch_databases_api_path(namespace);
    for segment in after_plural {
        route.push('/');
        route.push_str(segment);
    }
    Some(route)
}

#[cfg(test)]
mod tests {
    use std::{convert::Infallible, future::Ready};

    use super::*;

    fn stored(rest: &str) -> String {
        format!(
            "/apis/{}/{}/{rest}",
            BranchDatabase::group(&()),
            BranchDatabase::version(&())
        )
    }

    /// Every request the branch code makes for the stored kind, a create, a watch on one
    /// namespace, a listing across all of them, a read, a patch or a delete of one branch, lands
    /// on the operator's route for the same objects.
    #[test]
    fn branch_database_paths_go_to_the_operator_route() {
        assert_eq!(
            operator_route_path(&stored("namespaces/team/branchdatabases")).as_deref(),
            Some("/apis/operator.metalbear.co/v1/namespaces/team/branchdatabases")
        );
        assert_eq!(
            operator_route_path(&stored("namespaces/team/branchdatabases/pg-x7f3")).as_deref(),
            Some("/apis/operator.metalbear.co/v1/namespaces/team/branchdatabases/pg-x7f3")
        );
        assert_eq!(
            operator_route_path(&stored("branchdatabases")).as_deref(),
            Some("/apis/operator.metalbear.co/v1/branchdatabases")
        );
    }

    /// Only the branch kind moves: the per-engine kinds of older operators, other groups and the
    /// operator's own routes are left as they are, so nothing else changes destination.
    #[test]
    fn other_paths_are_left_alone() {
        for path in [
            stored("namespaces/team/pgbranchdatabases"),
            stored("namespaces/team/branchdatabasesx"),
            "/apis/operator.metalbear.co/v1/namespaces/team/branchdatabases".to_owned(),
            "/apis/profiles.mirrord.metalbear.co/v1alpha/mirrordclusterprofiles/dev".to_owned(),
            "/api/v1/namespaces/team/pods".to_owned(),
        ] {
            assert_eq!(operator_route_path(&path), None, "{path}");
        }
    }

    /// A service that answers with the URI it was called with.
    struct EchoUri;

    impl Service<Request<()>> for EchoUri {
        type Response = Uri;
        type Error = Infallible;
        type Future = Ready<Result<Uri, Infallible>>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, request: Request<()>) -> Self::Future {
            std::future::ready(Ok(request.uri().clone()))
        }
    }

    fn sent_uri(through_login: bool, uri: &str) -> Uri {
        let request = Request::get(uri).body(()).expect("valid test request");
        let Ok(sent) = LoginRoutesLayer::new(through_login)
            .layer(EchoUri)
            .call(request)
            .into_inner();
        sent
    }

    /// A login client sends a watch to the operator's route with its query intact, since the
    /// watch resumes from `resourceVersion`; a kubeconfig client keeps reading the stored group.
    #[test]
    fn only_a_login_client_is_rerouted_and_the_query_survives() {
        let watch = format!(
            "{}?watch=true&resourceVersion=42",
            stored("namespaces/team/branchdatabases")
        );

        assert_eq!(
            sent_uri(true, &watch),
            "/apis/operator.metalbear.co/v1/namespaces/team/branchdatabases?watch=true&resourceVersion=42"
        );
        assert_eq!(sent_uri(false, &watch), watch.as_str());
    }
}
