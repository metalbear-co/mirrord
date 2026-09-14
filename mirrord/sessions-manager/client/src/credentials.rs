//! Credentials sent with sessions-manager requests.
//!
//! [`SharedSecretCredentials`] proves a request may reach sessions-manager at all, to an
//! authenticating proxy or load balancer placed in front of it.
//!
//! This is not the per-assignment authorization the control plane hands out; that one is minted
//! per data-plane connection and travels with the assignment.

mod shared_secret;

use std::sync::Arc;

use futures::future::{BoxFuture, FutureExt};
use reqwest::header::HeaderMap;
pub use shared_secret::SharedSecretCredentials;

use crate::error::SessionsManagerClientError;

/// Supplies the headers that authenticate a client's sessions-manager requests.
///
/// The two planes are asked separately because they do not authenticate against the same thing.
/// The control plane is a plain HTTP/SSE API that has to establish the caller's identity from
/// scratch on every request; the data-plane upgrade instead presents the single-use credential
/// the control plane just minted for that one assignment, under `authorization`. A credential
/// that belongs to only one of the two would otherwise have to be filtered out by the transport
/// that must not send it.
///
/// Producing the headers is asynchronous because a provider may have to reach the network for
/// them. Both callers are already inside a cancellable, deadline-bounded async path, so a slow or
/// failing provider fails the attempt that needed it and is retried with it, rather than being
/// handled out of band.
pub trait CredentialProvider: Send + Sync {
    fn control_plane_headers(&self)
    -> BoxFuture<'_, Result<HeaderMap, SessionsManagerClientError>>;

    /// Defaults to [`CredentialProvider::control_plane_headers`], which is right for anything
    /// that authenticates the client to a fronting proxy: the proxy sees both planes and has to
    /// be satisfied by both.
    fn data_plane_headers(&self) -> BoxFuture<'_, Result<HeaderMap, SessionsManagerClientError>> {
        self.control_plane_headers()
    }
}

fn ready_headers(
    headers: HeaderMap,
) -> BoxFuture<'static, Result<HeaderMap, SessionsManagerClientError>> {
    std::future::ready(Ok(headers)).boxed()
}

#[derive(Default)]
pub(crate) struct NoCredentials;

impl CredentialProvider for NoCredentials {
    fn control_plane_headers(
        &self,
    ) -> BoxFuture<'_, Result<HeaderMap, SessionsManagerClientError>> {
        ready_headers(HeaderMap::new())
    }
}

/// The credentials a client uses unless the caller supplies its own: the shared secret when
/// the environment configures one, and nothing otherwise.
pub(crate) fn credentials_from_env()
-> Result<Arc<dyn CredentialProvider>, SessionsManagerClientError> {
    match SharedSecretCredentials::from_env()? {
        Some(credentials) => {
            tracing::debug!(
                header = %credentials.name,
                "authenticating sessions-manager connections with a shared secret"
            );
            Ok(Arc::new(credentials))
        }
        None => Ok(Arc::new(NoCredentials)),
    }
}
