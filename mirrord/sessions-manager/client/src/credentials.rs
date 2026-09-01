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

/// Header names the client sets for itself, which the shared secret may not take over.
///
/// Every one of these is applied after the credential headers, so a shared secret sent under
/// one of these names is either replaced or duplicated rather than delivered: `authorization`
/// carries the per-assignment token on the data plane, `accept` selects the control-plane
/// event stream, and the rest are part of the WebSocket handshake. The failure would other-
/// wise be near-invisible — the control plane authenticating while every data-plane upgrade
/// is rejected by the fronting proxy — so a name from this list is refused up front.
const RESERVED_HEADER_NAMES: &[&str] = &[
    "authorization",
    "accept",
    "host",
    "connection",
    "upgrade",
    "sec-websocket-key",
    "sec-websocket-version",
    "sec-websocket-protocol",
    "sec-websocket-extensions",
];

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
        // `HeaderName` parsing lowercases, so this comparison needs no normalizing of its own.
        if RESERVED_HEADER_NAMES.contains(&name.as_str()) {
            return Err(SessionsManagerClientError::InvalidConfig(format!(
                "auth header name {name} is reserved by the sessions-manager client"
            )));
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Exercises `new` rather than `from_env`, so the cases do not race each other over
    /// process environment.
    fn header_of(name: &str, token: &str) -> Result<HeaderMap, SessionsManagerClientError> {
        SharedSecretCredentials::new(name, token)?.headers()
    }

    #[test]
    fn sends_the_token_under_the_configured_name() {
        let headers = header_of("x-custom-auth", "shhh").unwrap();
        assert_eq!(headers.get("x-custom-auth").unwrap(), "shhh");
    }

    #[test]
    fn header_name_is_case_insensitive() {
        let headers = header_of("X-Custom-Auth", "shhh").unwrap();
        assert_eq!(headers.get("x-custom-auth").unwrap(), "shhh");
    }

    #[test]
    fn token_is_marked_sensitive_so_it_stays_out_of_logs() {
        let headers = header_of("x-custom-auth", "shhh").unwrap();
        assert!(headers.get("x-custom-auth").unwrap().is_sensitive());
    }

    /// A reserved name is refused outright: the client overwrites each of these after
    /// applying credentials, so the secret would never reach the proxy.
    #[test]
    fn reserved_header_names_are_refused() {
        for name in RESERVED_HEADER_NAMES {
            // `SharedSecretCredentials` holds a secret and deliberately has no `Debug`, so
            // the result is matched rather than unwrapped.
            let Err(error) = SharedSecretCredentials::new(name, "shhh") else {
                panic!("{name} should be refused as an auth header name");
            };
            assert!(
                matches!(error, SessionsManagerClientError::InvalidConfig(_)),
                "{name} produced {error:?}"
            );
        }
    }

    #[test]
    fn reserved_check_ignores_case() {
        assert!(
            SharedSecretCredentials::new("Authorization", "shhh").is_err(),
            "reserved names should be refused whatever their case"
        );
    }

    #[test]
    fn malformed_names_and_tokens_are_refused() {
        assert!(
            SharedSecretCredentials::new("no spaces allowed", "shhh").is_err(),
            "an invalid header name should be refused"
        );
        assert!(
            SharedSecretCredentials::new("x-custom-auth", "new\nline").is_err(),
            "a token that cannot be a header value should be refused"
        );
    }
}
