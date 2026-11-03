//! The token `mirrord login` gives the developer's CLI, with which an intproxy authenticates to
//! sessions-manager itself.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use futures::future::{BoxFuture, FutureExt};
use reqwest::header::{HeaderMap, HeaderValue};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use url::{Host, Url};

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
/// The token identifies the developer for as long as it lasts, so it is sent only where
/// [`LoginTokenCredentials::sends_token_to`] allows. Elsewhere requests go out without it, as
/// they would without a login: a sessions-manager that does not authenticate its callers still
/// works, and one that does refuses the session.
///
/// Control plane only, for the same reason as
/// [`CloudTokenCredentials`](super::CloudTokenCredentials): the data-plane upgrade authenticates
/// with the single-use credential minted for its assignment, which occupies this same header
/// name.
pub struct LoginTokenCredentials {
    source: Arc<dyn LoginTokenSource>,
    sends_token: bool,
    warned_withheld: AtomicBool,
}

impl LoginTokenCredentials {
    /// Credentials for requests to the sessions-manager at `base_url`.
    pub fn new(source: Arc<dyn LoginTokenSource>, base_url: &Url) -> Self {
        Self {
            source,
            sends_token: Self::sends_token_to(base_url),
            warned_withheld: AtomicBool::new(false),
        }
    }

    /// Whether the token may be sent to the sessions-manager at `base_url`: over TLS, or over
    /// plain HTTP to one on this machine, as when it is run locally or reached through a
    /// port-forward.
    pub fn sends_token_to(base_url: &Url) -> bool {
        base_url.scheme() == "https"
            || match base_url.host() {
                Some(Host::Domain(domain)) => domain == "localhost",
                Some(Host::Ipv4(ip)) => ip.is_loopback(),
                Some(Host::Ipv6(ip)) => ip.is_loopback(),
                None => false,
            }
    }
}

impl CredentialProvider for LoginTokenCredentials {
    fn control_plane_headers(
        &self,
    ) -> BoxFuture<'_, Result<HeaderMap, SessionsManagerClientError>> {
        async move {
            let mut headers = HeaderMap::new();
            let Some(token) = self.source.login_token().await? else {
                return Ok(headers);
            };
            if !self.sends_token {
                if !self.warned_withheld.swap(true, Ordering::Relaxed) {
                    tracing::warn!(
                        "Not sending the mirrord login token to sessions-manager over plain HTTP"
                    );
                }
                return Ok(headers);
            }
            let mut header = HeaderValue::from_str(&format!("Bearer {}", token.expose_secret()))
                .map_err(|_| SessionsManagerClientError::InvalidLoginToken)?;
            header.set_sensitive(true);
            headers.insert(reqwest::header::AUTHORIZATION, header);
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

#[cfg(test)]
mod tests {
    use reqwest::header::AUTHORIZATION;

    use super::*;

    struct FixedLogin(Option<&'static str>);

    impl LoginTokenSource for FixedLogin {
        fn login_token(
            &self,
        ) -> BoxFuture<'_, Result<Option<LoginToken>, SessionsManagerClientError>> {
            let token = self.0.map(|token| LoginToken::from(token.to_owned()));
            std::future::ready(Ok(token)).boxed()
        }
    }

    fn credentials(token: Option<&'static str>) -> LoginTokenCredentials {
        LoginTokenCredentials::new(
            Arc::new(FixedLogin(token)),
            &Url::parse("https://sm.example.com/sm").unwrap(),
        )
    }

    #[test]
    fn sends_the_token_only_over_tls_or_to_this_machine() {
        for (url, sends) in [
            ("https://sm.example.com/sm", true),
            ("http://localhost:8080/sm", true),
            ("http://127.0.0.1:8080", true),
            ("http://[::1]:8080", true),
            ("http://sm.example.com/sm", false),
            ("http://localhost.example.com", false),
            ("http://10.0.0.1:8080", false),
        ] {
            assert_eq!(
                LoginTokenCredentials::sends_token_to(&Url::parse(url).unwrap()),
                sends,
                "{url}"
            );
        }
    }

    /// Over plain HTTP to another host the request goes out as it would without a login, so a
    /// sessions-manager that does not authenticate its callers keeps working.
    #[tokio::test]
    async fn withholds_the_token_over_plain_http_to_another_host() {
        let credentials = LoginTokenCredentials::new(
            Arc::new(FixedLogin(Some("abc"))),
            &Url::parse("http://sm.example.com/sm").unwrap(),
        );

        let headers = credentials.control_plane_headers().await.unwrap();

        assert!(headers.is_empty());
    }

    #[tokio::test]
    async fn sends_the_token_as_a_sensitive_bearer_on_the_control_plane() {
        let headers = credentials(Some("abc"))
            .control_plane_headers()
            .await
            .unwrap();

        let authorization = headers.get(AUTHORIZATION).unwrap();
        assert_eq!(authorization, "Bearer abc");
        assert!(authorization.is_sensitive());
    }

    /// The data-plane upgrade carries the assignment's own credential in `Authorization`, which
    /// the login token must not replace.
    #[tokio::test]
    async fn sends_nothing_on_the_data_plane() {
        let headers = credentials(Some("abc")).data_plane_headers().await.unwrap();

        assert!(headers.is_empty());
    }

    #[tokio::test]
    async fn sends_nothing_without_a_login() {
        let headers = credentials(None).control_plane_headers().await.unwrap();

        assert!(headers.is_empty());
    }

    #[test]
    fn debug_output_does_not_contain_the_token() {
        let token = LoginToken::from("secret-token".to_owned());

        assert!(!format!("{token:?}").contains("secret-token"));
    }
}
