//! The token `mirrord login` gives the developer's CLI, with which an intproxy authenticates to
//! sessions-manager itself.

use std::sync::Arc;

use futures::future::{BoxFuture, FutureExt};
use reqwest::header::{HeaderMap, HeaderValue};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::{CredentialProvider, ready_headers};
use crate::error::SessionsManagerClientError;

/// A signed `mirrord login` token.
///
/// It serializes as the bare token, which is how the CLI's token store keeps it. It is held as a
/// [`SecretString`], so it stays out of logs and reading it is explicit.
#[derive(Debug)]
pub struct LoginToken(SecretString);

impl LoginToken {
    pub fn expose_secret(&self) -> &str {
        self.0.expose_secret()
    }
}

impl From<String> for LoginToken {
    fn from(value: String) -> Self {
        Self(SecretString::from(value))
    }
}

impl Serialize for LoginToken {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.expose_secret())
    }
}

impl<'de> Deserialize<'de> for LoginToken {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self::from)
    }
}

/// Where [`LoginTokenCredentials`] gets the developer's current [`LoginToken`].
///
/// The token store belongs to the CLI, which implements this. Every process that presents the
/// token (the CLI itself, the intproxy and the extproxy) runs on the developer's machine as the
/// developer, so each reads the store itself instead of receiving the token from the CLI.
pub trait LoginTokenSource: Send + Sync {
    /// The token to present now, or [`None`] when the developer is not logged in.
    fn login_token(&self) -> BoxFuture<'_, Result<Option<LoginToken>, SessionsManagerClientError>>;
}

/// Authenticates an intproxy to sessions-manager with the developer's [`LoginToken`], sent as
/// `Authorization: Bearer <token>`. Agents authenticate with
/// [`CloudTokenCredentials`](super::CloudTokenCredentials) instead: sessions-manager accepts each
/// token only for its own role.
///
/// The token is read from its [`LoginTokenSource`] for every request, so a session that outlives
/// its token picks up the one from a later `mirrord login` when it reconnects. It is not verified
/// or refreshed here: sessions-manager verifies it, and a developer whose token expired runs
/// `mirrord login` again. Without a token, requests go out unauthenticated, for a
/// sessions-manager that does not authenticate its callers.
///
/// Control plane only, for the same reason as
/// [`CloudTokenCredentials`](super::CloudTokenCredentials): the data-plane upgrade authenticates
/// with the single-use credential minted for its assignment, which occupies this same header
/// name.
pub struct LoginTokenCredentials {
    source: Arc<dyn LoginTokenSource>,
}

impl LoginTokenCredentials {
    pub fn new(source: Arc<dyn LoginTokenSource>) -> Self {
        Self { source }
    }
}

impl CredentialProvider for LoginTokenCredentials {
    fn control_plane_headers(
        &self,
    ) -> BoxFuture<'_, Result<HeaderMap, SessionsManagerClientError>> {
        async move {
            let mut headers = HeaderMap::new();
            if let Some(token) = self.source.login_token().await? {
                let mut header =
                    HeaderValue::from_str(&format!("Bearer {}", token.expose_secret()))
                        .map_err(|_| SessionsManagerClientError::InvalidLoginToken)?;
                header.set_sensitive(true);
                headers.insert(reqwest::header::AUTHORIZATION, header);
            }
            Ok(headers)
        }
        .boxed()
    }

    fn data_plane_headers(&self) -> BoxFuture<'_, Result<HeaderMap, SessionsManagerClientError>> {
        ready_headers(HeaderMap::new())
    }

    fn unauthorized_error(&self) -> Option<SessionsManagerClientError> {
        Some(SessionsManagerClientError::LoginTokenRejected)
    }
}
