use std::{env::VarError, sync::Arc};

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

use crate::error::SessionsManagerClientError;

pub trait CredentialProvider: Send + Sync {
    fn headers(&self) -> Result<HeaderMap, SessionsManagerClientError>;
}

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

/// Names the header carrying [`SESSIONS_MANAGER_AUTH_TOKEN_ENV`]. Only needed when the
/// deployment expects something other than [`DEFAULT_AUTH_HEADER_NAME`].
pub const SESSIONS_MANAGER_AUTH_HEADER_ENV: &str = "MIRRORD_SESSIONS_MANAGER_AUTH_HEADER";

const DEFAULT_AUTH_HEADER_NAME: &str = "x-mirrord-sm-auth";

/// Sends a fixed shared secret on every sessions-manager request, for deployments that put
/// an authenticating proxy or load balancer in front of it.
///
/// This is distinct from the per-assignment authorization the control plane hands out: the
/// proxy decides whether a request reaches sessions-manager at all, and has to make that
/// call without understanding the control-plane or data-plane protocol.
pub struct SharedSecretCredentials {
    name: HeaderName,
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

        let name = std::env::var(SESSIONS_MANAGER_AUTH_HEADER_ENV)
            .unwrap_or_else(|_| DEFAULT_AUTH_HEADER_NAME.to_owned());

        Self::new(&name, &token).map(Some)
    }

    pub fn new(name: &str, token: &str) -> Result<Self, SessionsManagerClientError> {
        let name = HeaderName::try_from(name).map_err(|_| {
            SessionsManagerClientError::InvalidConfig(format!("invalid auth header name: {name}"))
        })?;
        let mut value = HeaderValue::from_str(token).map_err(|_| {
            SessionsManagerClientError::InvalidConfig(
                "auth token is not a valid header value".to_owned(),
            )
        })?;
        value.set_sensitive(true);

        Ok(Self { name, value })
    }
}

impl CredentialProvider for SharedSecretCredentials {
    fn headers(&self) -> Result<HeaderMap, SessionsManagerClientError> {
        let mut headers = HeaderMap::new();
        headers.insert(self.name.clone(), self.value.clone());
        Ok(headers)
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
