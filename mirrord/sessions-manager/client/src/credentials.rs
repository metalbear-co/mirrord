//! Credentials sent with sessions-manager requests.
//!
//! Two independent mechanisms live here, and a deployment may use either, both, or neither:
//!
//! - [`SharedSecretCredentials`] proves a request may reach sessions-manager at all, to an
//!   authenticating proxy or load balancer placed in front of it.
//! - [`CloudTokenCredentials`] proves *who* is calling to sessions-manager itself, by exchanging a
//!   long-lived MetalBear API key for a short-lived token.
//!
//! Neither is the per-assignment authorization the control plane hands out; that one is minted
//! per data-plane connection and travels with the assignment.

mod cloud_token;
mod shared_secret;

use std::sync::Arc;

pub use cloud_token::CloudTokenCredentials;
use futures::future::{BoxFuture, FutureExt};
use reqwest::header::HeaderMap;
pub use shared_secret::SharedSecretCredentials;

use crate::error::SessionsManagerClientError;

/// Supplies the headers that authenticate a client's sessions-manager requests.
///
/// The two planes are asked separately because they do not authenticate against the same thing.
/// The control plane is a plain HTTP/SSE API that has to establish the caller's identity from
/// scratch on every request; the data-plane upgrade sends its headers alongside the single-use
/// credential the control plane minted for that one assignment, which is always sent under
/// `authorization` and replaces any `authorization` header the provider returns. A credential
/// that belongs to only one of the two would otherwise have to be filtered out by the transport
/// that must not send it.
///
/// Producing the headers is asynchronous because a provider may have to reach the network for
/// them (see [`CloudTokenCredentials`]). Both callers are already inside a deadline-bounded async
/// path, so a slow or failing provider fails the attempt that needed it and is retried with it,
/// rather than being handled out of band.
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

/// Supplies no headers for directly reachable sessions-manager deployments.
#[derive(Default)]
pub(crate) struct NoCredentials;

impl CredentialProvider for NoCredentials {
    fn control_plane_headers(
        &self,
    ) -> BoxFuture<'_, Result<HeaderMap, SessionsManagerClientError>> {
        ready_headers(HeaderMap::new())
    }
}

/// Whatever the environment configures, which may be both mechanisms at once: they answer to
/// different parties (a fronting proxy, and sessions-manager itself) and are enabled by
/// different variables, so neither implies nor excludes the other.
struct EnvCredentials {
    shared_secret: Option<SharedSecretCredentials>,
    cloud: Option<CloudTokenCredentials>,
}

impl CredentialProvider for EnvCredentials {
    fn control_plane_headers(
        &self,
    ) -> BoxFuture<'_, Result<HeaderMap, SessionsManagerClientError>> {
        async move {
            let mut headers = HeaderMap::new();
            if let Some(shared_secret) = &self.shared_secret {
                headers.extend(shared_secret.control_plane_headers().await?);
            }
            if let Some(cloud) = &self.cloud {
                headers.extend(cloud.control_plane_headers().await?);
            }
            Ok(headers)
        }
        .boxed()
    }

    fn data_plane_headers(&self) -> BoxFuture<'_, Result<HeaderMap, SessionsManagerClientError>> {
        async move {
            let mut headers = HeaderMap::new();
            if let Some(shared_secret) = &self.shared_secret {
                headers.extend(shared_secret.data_plane_headers().await?);
            }
            if let Some(cloud) = &self.cloud {
                headers.extend(cloud.data_plane_headers().await?);
            }
            Ok(headers)
        }
        .boxed()
    }
}

/// The credentials a client uses unless the caller supplies its own.
pub(crate) fn credentials_from_env()
-> Result<Arc<dyn CredentialProvider>, SessionsManagerClientError> {
    let shared_secret = SharedSecretCredentials::from_env()?;
    let cloud = CloudTokenCredentials::from_env()?;

    if shared_secret.is_some() {
        tracing::debug!("authenticating sessions-manager connections with a shared secret");
    }
    if let Some(cloud) = &cloud {
        tracing::debug!(
            endpoint = %cloud.endpoint,
            "authenticating sessions-manager control-plane requests with a MetalBear cloud token"
        );
    }

    match (shared_secret, cloud) {
        (None, None) => Ok(Arc::new(NoCredentials)),
        (shared_secret, cloud) => Ok(Arc::new(EnvCredentials {
            shared_secret,
            cloud,
        })),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::http::StatusCode;

    use super::{
        cloud_token::{
            SESSIONS_MANAGER_API_KEY_ENV,
            tests::{TokenEndpoint, token},
        },
        shared_secret::{AUTH_HEADER_NAME, SESSIONS_MANAGER_AUTH_TOKEN_ENV},
        *,
    };

    /// Both mechanisms configured at once: the proxy secret goes to both planes, the cloud token
    /// only to the control plane.
    #[tokio::test]
    async fn both_mechanisms_can_be_configured_at_once() {
        let endpoint =
            TokenEndpoint::start([(StatusCode::OK, token(Duration::from_secs(600)))]).await;
        let credentials = EnvCredentials {
            shared_secret: Some(SharedSecretCredentials::new("shhh").unwrap()),
            cloud: Some(endpoint.credentials()),
        };

        let control_plane = credentials.control_plane_headers().await.unwrap();
        let data_plane = credentials.data_plane_headers().await.unwrap();

        assert_eq!(control_plane.get(AUTH_HEADER_NAME).unwrap(), "shhh");
        assert!(control_plane.contains_key(reqwest::header::AUTHORIZATION));
        assert_eq!(data_plane.get(AUTH_HEADER_NAME).unwrap(), "shhh");
        assert!(!data_plane.contains_key(reqwest::header::AUTHORIZATION));
    }

    /// Without an API key the provider is not built at all, and a client with neither mechanism
    /// configured sends no credential headers.
    #[tokio::test]
    async fn is_a_no_op_when_the_api_key_is_unset() {
        for name in [
            SESSIONS_MANAGER_API_KEY_ENV,
            SESSIONS_MANAGER_AUTH_TOKEN_ENV,
        ] {
            assert!(
                std::env::var(name).is_err(),
                "{name} must not be set while these tests run"
            );
        }

        assert!(CloudTokenCredentials::from_env().unwrap().is_none());
        assert!(
            credentials_from_env()
                .unwrap()
                .control_plane_headers()
                .await
                .unwrap()
                .is_empty()
        );
    }
}
