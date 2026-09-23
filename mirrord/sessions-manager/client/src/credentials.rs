use std::{env::VarError, sync::Arc};

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

use crate::error::SessionsManagerClientError;

pub trait CredentialProvider: Send + Sync {
    fn headers(&self) -> Result<HeaderMap, SessionsManagerClientError>;
}

/// Supplies no headers for directly reachable sessions-manager deployments.
#[derive(Default)]
pub(crate) struct NoCredentials;

impl CredentialProvider for NoCredentials {
    fn headers(&self) -> Result<HeaderMap, SessionsManagerClientError> {
        Ok(HeaderMap::new())
    }
}

/// Shared secret expected by whatever fronts sessions-manager. Setting it is what
/// turns [`SharedSecretCredentials`] on.
pub const SESSIONS_MANAGER_AUTH_TOKEN_ENV: &str = "MIRRORD_SESSIONS_MANAGER_AUTH_TOKEN";

const AUTH_HEADER_NAME: HeaderName = HeaderName::from_static("x-mirrord-sm-auth");

/// Sends a fixed shared secret on every sessions-manager request, for deployments that put
/// an authenticating proxy or load balancer in front of it.
///
/// This is distinct from the per-assignment authorization the control plane hands out: the
/// proxy decides whether a request reaches sessions-manager at all, and has to make that
/// call without understanding the control-plane or data-plane protocol.
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
}

impl CredentialProvider for SharedSecretCredentials {
    fn headers(&self) -> Result<HeaderMap, SessionsManagerClientError> {
        let mut headers = HeaderMap::new();
        headers.insert(AUTH_HEADER_NAME, self.value.clone());
        Ok(headers)
    }
}

/// The credentials a client uses unless the caller supplies its own: the shared secret when
/// the environment configures one, and nothing otherwise.
pub(crate) fn credentials_from_env()
-> Result<Arc<dyn CredentialProvider>, SessionsManagerClientError> {
    match SharedSecretCredentials::from_env()? {
        Some(credentials) => {
            tracing::debug!("authenticating sessions-manager connections with a shared secret");
            Ok(Arc::new(credentials))
        }
        None => Ok(Arc::new(NoCredentials)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers_for(token: &str) -> Result<HeaderMap, SessionsManagerClientError> {
        SharedSecretCredentials::new(token)?.headers()
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
