//! Wire format of the operator's single sign-on login, shared by the CLI and the operator.
//!
//! A developer logs in with `mirrord login --url <operator url>`. The CLI and the operator run an
//! authorization code flow with PKCE where the operator hosts the OIDC redirect, since it holds the
//! client secret: the CLI opens the browser at [`AUTHORIZE_PATH`], the provider sends the browser
//! back to the operator's [`CALLBACK_PATH`], the operator hands the browser a short-lived grant for
//! the CLI's loopback callback, and the CLI exchanges the grant and its PKCE verifier at
//! [`TOKEN_PATH`] for a login token it then sends as a bearer token on every request.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Path of the login routes under the operator's URL, the one the chart's `sso.url` names. The
/// Ingress for that URL forwards everything under `/apis/operator.metalbear.co/`.
pub const SSO_PATH: &str = "apis/operator.metalbear.co/sso";

/// `GET`: answers [`SsoLoginInfo`] when this operator accepts CLI logins, the paid-feature gate
/// when its license does not include SSO. The CLI calls it before opening a browser.
pub const INFO_PATH: &str = "login";

/// `GET`, opened in the browser with `code_challenge`, `code_challenge_method=S256`,
/// `redirect_uri` (the CLI's loopback callback) and `state`: redirects to the identity provider.
pub const AUTHORIZE_PATH: &str = "authorize";

/// `GET`, the identity provider's redirect target: exchanges the code and redirects the browser to
/// the CLI's loopback callback with a `grant` and the CLI's `state`.
pub const CALLBACK_PATH: &str = "callback";

/// `POST` [`SsoTokenRequest`]: answers [`SsoTokenResponse`].
pub const TOKEN_PATH: &str = "token";

/// `GET` with a `status` query param: the page the CLI's loopback callback sends the browser to
/// once it received the grant, so the developer sees that the login is done.
pub const DONE_PATH: &str = "done";

/// The PKCE challenge method the operator accepts, RFC 7636 section 4.2.
pub const CODE_CHALLENGE_METHOD: &str = "S256";

/// What the operator says about itself to a CLI about to log in.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SsoLoginInfo {
    /// Version of the operator, for the CLI's messages.
    pub operator_version: String,
    /// The identity provider developers sign in with, as the chart's `sso.issuerUrl` names it.
    pub issuer_url: String,
}

/// What the CLI sends to exchange the grant for a login token.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SsoTokenRequest {
    /// The grant the browser delivered to the CLI's loopback callback.
    pub grant: String,
    /// The PKCE verifier whose S256 challenge the grant is bound to.
    pub code_verifier: String,
    /// The loopback callback the grant was issued for, as the CLI passed it to the authorize route.
    pub redirect_uri: String,
}

/// The login token and who it identifies.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SsoTokenResponse {
    /// Bearer token for the operator's URL, valid until `expires_at`.
    pub token: OperatorLoginToken,
    /// The Kubernetes username the operator derived from the identity provider's claims, the one
    /// RoleBindings and session ownership see.
    pub username: String,
    /// What the developer recognizes as their account: the token's email when the provider sent
    /// one, else the username.
    pub account: String,
    /// When the token stops being accepted.
    pub expires_at: DateTime<Utc>,
}

/// A login token as the CLI stores and sends it. Printing it with [`fmt::Debug`] redacts it.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct OperatorLoginToken(String);

impl OperatorLoginToken {
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl From<String> for OperatorLoginToken {
    fn from(token: String) -> Self {
        Self(token)
    }
}

impl fmt::Debug for OperatorLoginToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// A saved login to one operator, what every command that reaches the operator through its URL
/// instead of a kubeconfig starts from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorLogin {
    /// The operator's URL (scheme, host and port, no trailing slash), as `mirrord login --url`
    /// normalized it.
    pub url: String,
    pub token: OperatorLoginToken,
    /// See [`SsoTokenResponse::username`].
    pub username: String,
    /// See [`SsoTokenResponse::account`].
    pub account: String,
    pub expires_at: DateTime<Utc>,
}

impl OperatorLogin {
    /// The host the developer logged in to, with the port when it is not the scheme's default:
    /// how messages and `mirrord whoami` name the operator.
    pub fn host(&self) -> &str {
        self.url
            .split_once("://")
            .map(|(_, rest)| rest)
            .unwrap_or(&self.url)
    }

    /// Whether the token is still accepted at `now`.
    pub fn is_valid_at(&self, now: DateTime<Utc>) -> bool {
        self.expires_at > now
    }

    /// A client configuration that reaches the operator at its URL with the login token as the
    /// bearer token, in place of one read from a kubeconfig. The URL came from the developer, so
    /// an unparsable one is an error rather than a panic.
    #[cfg(feature = "client")]
    pub fn kube_config(&self) -> Result<kube::Config, http::uri::InvalidUri> {
        let mut config = kube::Config::new(self.url.parse()?);
        config.auth_info.token = Some(self.token.expose_secret().to_owned().into());
        config.default_retry = false;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;

    use super::*;

    fn login(url: &str) -> OperatorLogin {
        OperatorLogin {
            url: url.to_owned(),
            token: OperatorLoginToken::from("secret-token".to_owned()),
            username: "dev@corp.example.com".to_owned(),
            account: "dev@corp.example.com".to_owned(),
            expires_at: Utc::now() + TimeDelta::hours(1),
        }
    }

    #[test]
    fn host_drops_the_scheme_and_keeps_a_port() {
        assert_eq!(
            login("https://mirrord.corp.example.com").host(),
            "mirrord.corp.example.com"
        );
        assert_eq!(login("https://localhost:8443").host(), "localhost:8443");
    }

    #[test]
    fn debug_output_does_not_contain_the_token() {
        let login = login("https://mirrord.corp.example.com");
        assert!(!format!("{login:?}").contains("secret-token"));
        assert!(!format!("{:?}", login.token).contains("secret-token"));
    }

    #[cfg(feature = "client")]
    #[test]
    fn kube_config_targets_the_operator_url_with_the_token() {
        let config = login("https://mirrord.corp.example.com")
            .kube_config()
            .unwrap();
        assert_eq!(
            config.cluster_url.to_string(),
            "https://mirrord.corp.example.com/"
        );
        assert!(config.auth_info.token.is_some());
        assert!(login("not a url").kube_config().is_err());
    }
}
