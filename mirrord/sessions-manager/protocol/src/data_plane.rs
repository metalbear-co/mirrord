use std::fmt;

use http::Uri;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use url::Url;

use crate::error::SessionsManagerProtocolError;

/// A credential whose value is exposed only at an explicit transport boundary.
///
/// Serialization deliberately exposes the credential because [`ConnectionAssignment`] is the wire
/// format between the sessions manager and its peers. Callers must only serialize it directly
/// into that authenticated control-plane response and must not log or persist the result.
#[derive(Clone)]
pub struct DataPlaneAuthorization(SecretString);

impl DataPlaneAuthorization {
    pub fn new(value: String) -> Self {
        Self(SecretString::from(value))
    }
}

impl ExposeSecret<str> for DataPlaneAuthorization {
    fn expose_secret(&self) -> &str {
        self.0.expose_secret()
    }
}

impl PartialEq for DataPlaneAuthorization {
    fn eq(&self, other: &Self) -> bool {
        self.expose_secret() == other.expose_secret()
    }
}

impl Eq for DataPlaneAuthorization {}

impl fmt::Debug for DataPlaneAuthorization {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

impl fmt::Display for DataPlaneAuthorization {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

impl Serialize for DataPlaneAuthorization {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.expose_secret())
    }
}

impl<'de> Deserialize<'de> for DataPlaneAuthorization {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer).map(Self::new)
    }
}
/// Relative data-plane URI resolved against the control-plane URL used by the receiving peer.
///
/// The current sessions-manager contract requires this endpoint to be an absolute path on the
/// control-plane origin. The authorization in an assignment is forwarded to the resolved endpoint.
/// Supporting another origin requires an explicit protocol change rather than an ambiguous URI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataPlaneEndpoint(Uri);

impl DataPlaneEndpoint {
    pub fn new(uri: Uri) -> Result<Self, SessionsManagerProtocolError> {
        if uri.scheme().is_some() || uri.authority().is_some() {
            return Err(SessionsManagerProtocolError::InvalidDataPlaneEndpointRecv(
                uri,
            ));
        }

        let path = uri
            .path_and_query()
            .ok_or(SessionsManagerProtocolError::InvalidDataPlaneEndpointRecv(
                uri.clone(),
            ))?
            .as_str();
        if !path.starts_with('/')
            || path.starts_with("//")
            || path.contains('\\')
            || path.contains("..") {
            return Err(SessionsManagerProtocolError::InvalidDataPlaneEndpointRecv(
                uri,
            ));
        }

        Ok(Self(uri))
    }

    pub fn resolve(
        &self,
        base_url: &Url,
        scheme: &str,
    ) -> Result<Url, SessionsManagerProtocolError> {
        let mut url = base_url.join(self.as_str()).map_err(|_| {
            SessionsManagerProtocolError::InvalidDataPlaneEndpointRes(base_url.clone())
        })?;
        url.set_scheme(scheme)
            .map_err(|_| SessionsManagerProtocolError::InvalidDataPlaneEndpointRes(url.clone()))?;
        if url.host_str().is_none()
            || url.host_str() != base_url.host_str()
            || url.port_or_known_default() != base_url.port_or_known_default()
        {
            return Err(SessionsManagerProtocolError::InvalidDataPlaneEndpointRes(
                url,
            ));
        }
        Ok(url)
    }

    pub fn as_str(&self) -> &str {
        self.0
            .path_and_query()
            .expect("validated data-plane endpoint has a path and query")
            .as_str()
    }
}

impl Serialize for DataPlaneEndpoint {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for DataPlaneEndpoint {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let uri: Uri = String::deserialize(deserializer)
            .and_then(|value| value.parse().map_err(serde::de::Error::custom))?;
        Self::new(uri).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use http::Uri;
    use url::Url;

    use super::DataPlaneEndpoint;
    use crate::SessionsManagerProtocolError;

    #[test]
    fn rejects_backslash_paths() {
        let endpoint = Uri::from_static(r"/\attacker.example/ws");

        assert!(matches!(
            DataPlaneEndpoint::new(endpoint),
            Err(SessionsManagerProtocolError::InvalidDataPlaneEndpointRecv(
                _
            ))
        ));
    }

    #[test]
    fn rejects_endpoints_that_resolve_to_another_origin() {
        let endpoint = DataPlaneEndpoint(Uri::from_static(r"/\attacker.example/ws"));
        let base_url = Url::parse("http://sessions-manager.example/sm/").unwrap();

        assert!(matches!(
            endpoint.resolve(&base_url, "ws"),
            Err(SessionsManagerProtocolError::InvalidDataPlaneEndpointRes(_))
        ));
    }

    #[test]
    fn rejects_path_traversal_sequences() {
        let endpoint = Uri::from_static("/../etc/passwd");

        assert!(matches!(
            DataPlaneEndpoint::new(endpoint),
            Err(SessionsManagerProtocolError::InvalidDataPlaneEndpointRecv(_))
        ));
    }

    #[test]
    fn rejects_double_dot_in_path() {
        let endpoint = Uri::from_static("/foo/../bar");

        assert!(matches!(
            DataPlaneEndpoint::new(endpoint),
            Err(SessionsManagerProtocolError::InvalidDataPlaneEndpointRecv(_))
        ));
    }
}
