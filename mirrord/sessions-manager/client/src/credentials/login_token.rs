//! The token `mirrord login` gives the developer's CLI, with which an intproxy authenticates to
//! sessions-manager itself.

use std::fmt;

use futures::future::BoxFuture;
use reqwest::header::{HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};

use super::{CredentialProvider, ready_headers};
use crate::error::SessionsManagerClientError;

/// A signed `mirrord login` token.
///
/// The CLI reads it from its token store and hands it to the intproxy inside
/// [`SessionsManagerConnectInfo`](crate::SessionsManagerConnectInfo), so it serializes as the
/// bare token. It must never appear in logs, so [`fmt::Debug`] does not print it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LoginToken(String);

impl LoginToken {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for LoginToken {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl fmt::Debug for LoginToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Authenticates an intproxy to sessions-manager with the developer's [`LoginToken`], sent as
/// `Authorization: Bearer <token>`. Agents authenticate with
/// [`CloudTokenCredentials`](super::CloudTokenCredentials) instead: sessions-manager accepts each
/// token only for its own role.
///
/// The token is not verified or refreshed here. sessions-manager verifies it, and a developer
/// whose token expired runs `mirrord login` again.
///
/// Control plane only, for the same reason as
/// [`CloudTokenCredentials`](super::CloudTokenCredentials): the data-plane upgrade authenticates
/// with the single-use credential minted for its assignment, which occupies this same header
/// name.
pub struct LoginTokenCredentials {
    /// Complete `Bearer <token>` value, marked sensitive.
    header: HeaderValue,
}

impl LoginTokenCredentials {
    pub fn new(token: &LoginToken) -> Result<Self, SessionsManagerClientError> {
        let mut header = HeaderValue::from_str(&format!("Bearer {}", token.as_str()))
            .map_err(|_| SessionsManagerClientError::InvalidLoginToken)?;
        header.set_sensitive(true);

        Ok(Self { header })
    }
}

impl CredentialProvider for LoginTokenCredentials {
    fn control_plane_headers(
        &self,
    ) -> BoxFuture<'_, Result<HeaderMap, SessionsManagerClientError>> {
        let mut headers = HeaderMap::new();
        headers.insert(reqwest::header::AUTHORIZATION, self.header.clone());
        ready_headers(headers)
    }

    fn data_plane_headers(&self) -> BoxFuture<'_, Result<HeaderMap, SessionsManagerClientError>> {
        ready_headers(HeaderMap::new())
    }
}
