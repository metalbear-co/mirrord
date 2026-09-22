//! A fixed shared secret for whatever fronts sessions-manager.

use std::env::VarError;

use futures::future::BoxFuture;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

use super::{CredentialProvider, ready_headers};
use crate::error::SessionsManagerClientError;

/// Shared secret expected by whatever fronts sessions-manager. Setting it is what
/// turns [`SharedSecretCredentials`] on.
pub const SESSIONS_MANAGER_AUTH_TOKEN_ENV: &str = "MIRRORD_SESSIONS_MANAGER_AUTH_TOKEN";

const AUTH_HEADER_NAME: HeaderName = HeaderName::from_static("x-mirrord-sm-auth");

/// Sends a fixed shared secret on every sessions-manager request, for deployments that put
/// an authenticating proxy or load balancer in front of it.
pub struct SharedSecretCredentials {
    value: HeaderValue,
}

impl SharedSecretCredentials {
    /// Reads the shared secret from the environment.
    ///
    /// [`None`] when no token is set, which is the case for a directly reachable
    /// sessions-manager.
    pub fn from_env() -> Result<Option<Self>, SessionsManagerClientError> {
        let token = match std::env::var(SESSIONS_MANAGER_AUTH_TOKEN_ENV) {
            Ok(token) => token,
            Err(VarError::NotPresent) => return Ok(None),
            Err(error) => return Err(error.into()),
        };

        Self::new(&token).map(Some)
    }

    fn new(token: &str) -> Result<Self, SessionsManagerClientError> {
        let mut value = HeaderValue::from_str(token)
            .map_err(|_| SessionsManagerClientError::InvalidSharedSecret)?;
        value.set_sensitive(true);

        Ok(Self { value })
    }

    fn header_map(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(AUTH_HEADER_NAME, self.value.clone());
        headers
    }
}

impl CredentialProvider for SharedSecretCredentials {
    fn control_plane_headers(
        &self,
    ) -> BoxFuture<'_, Result<HeaderMap, SessionsManagerClientError>> {
        ready_headers(self.header_map())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers_for(token: &str) -> Result<HeaderMap, SessionsManagerClientError> {
        Ok(SharedSecretCredentials::new(token)?.header_map())
    }

    #[test]
    fn sends_the_token_under_the_fixed_header_name() {
        let headers = headers_for("shhh").unwrap();
        assert_eq!(headers.get(AUTH_HEADER_NAME).unwrap(), "shhh");
    }

    #[test]
    fn token_is_marked_sensitive_so_it_stays_out_of_logs() {
        let headers = headers_for("shhh").unwrap();
        assert!(headers.get(AUTH_HEADER_NAME).unwrap().is_sensitive());
    }

    #[test]
    fn malformed_tokens_are_refused() {
        assert!(
            matches!(
                SharedSecretCredentials::new("new\nline"),
                Err(SessionsManagerClientError::InvalidSharedSecret)
            ),
            "a token that cannot be a header value should be refused"
        );
    }
}
