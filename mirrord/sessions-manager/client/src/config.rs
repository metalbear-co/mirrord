use std::{env::VarError, ops::Not};

use mirrord_sessions_manager_protocol::ServiceScope;
use url::Url;

use crate::error::SessionsManagerClientError;

/// Complete sessions-manager API base URL, including any deployment-specific path prefix.
///
/// For example, both `https://example.com/sm` and `https://sm.example.com` are valid; API version
/// and resource segments are appended to the configured path.
pub(crate) const SESSIONS_MANAGER_URL_ENV: &str = "MIRRORD_SESSIONS_MANAGER_URL";

/// Identifies a sessions-manager service scope and its control-plane base URL.
#[derive(Clone)]
pub(crate) struct SessionsManagerConfig {
    pub(crate) scope: ServiceScope,
    pub(crate) base_url: Url,
}

impl SessionsManagerConfig {
    /// Builds a config from an already-resolved `base_url` (see
    /// [`SessionsManagerConfig::base_url_from_env`]), so construction doesn't implicitly depend on
    /// process environment and can be tested or driven programmatically.
    pub(crate) fn new(
        scope: ServiceScope,
        base_url: Url,
    ) -> Result<Self, SessionsManagerClientError> {
        scope
            .environment
            .trim()
            .is_empty()
            .not()
            .ok_or(SessionsManagerClientError::MissingConfigEnvironment)?;
        scope
            .service
            .trim()
            .is_empty()
            .not()
            .ok_or(SessionsManagerClientError::MissingConfigService)?;

        Ok(Self { scope, base_url })
    }

    /// Reads the sessions-manager base URL from [`SESSIONS_MANAGER_URL_ENV`] and
    /// validates/normalizes it.
    pub(crate) fn base_url_from_env() -> Result<Url, SessionsManagerClientError> {
        let raw = match std::env::var(SESSIONS_MANAGER_URL_ENV) {
            Ok(raw) => raw,
            Err(VarError::NotPresent) => {
                return Err(SessionsManagerClientError::MissingSessionsManagerUrl);
            }
            Err(error) => return Err(error.into()),
        };

        Self::parse_base_url(&raw)
    }

    fn parse_base_url(raw: &str) -> Result<Url, SessionsManagerClientError> {
        let mut base_url = Url::parse(raw)?;
        if !matches!(base_url.scheme(), "http" | "https")
            || base_url.cannot_be_a_base()
            || base_url.host().is_none()
            || base_url.query().is_some()
            || base_url.fragment().is_some()
        {
            return Err(SessionsManagerClientError::InvalidBaseUrl);
        }

        if !base_url.path().ends_with('/') {
            let path = format!("{}/", base_url.path());
            base_url.set_path(&path);
        }

        Ok(base_url)
    }
}
