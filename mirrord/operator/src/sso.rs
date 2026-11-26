//! Wire format of the operator's single sign-on login, shared by the CLI and the operator.
//!
//! A developer logs in with `mirrord login --url <operator url>`. The CLI and the operator run an
//! authorization code flow with PKCE where the operator hosts the OIDC redirect, since it holds the
//! client secret: the CLI opens the browser at [`AUTHORIZE_PATH`](crate::sso::AUTHORIZE_PATH), the provider sends the browser
//! back to the operator's [`CALLBACK_PATH`](crate::sso::CALLBACK_PATH), the operator hands the browser a short-lived grant for
//! the CLI's loopback callback, and the CLI exchanges the grant and its PKCE verifier at
//! [`TOKEN_PATH`](crate::sso::TOKEN_PATH) for a login token it then sends as a bearer token on every request.

use std::{collections::BTreeMap, fmt, ops::Not};

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
        host_of(&self.url)
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

/// The host and port of an operator URL, how messages name the operator.
pub fn host_of(url: &str) -> &str {
    url.split_once("://").map(|(_, rest)| rest).unwrap_or(url)
}

/// A login to one operator as `~/.mirrord/auth.json` stores it, keyed there by the operator URL.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredOperatorLogin {
    pub token: OperatorLoginToken,
    /// Kubernetes username the operator derived from the identity provider's claims.
    pub username: String,
    /// The account as the developer knows it, shown by `mirrord whoami`.
    pub account: String,
    /// When the operator stops accepting the token.
    pub expires_at: DateTime<Utc>,
    /// When `mirrord login` saved this entry; the latest one takes over when the active login is
    /// removed.
    pub logged_in_at: DateTime<Utc>,
}

impl StoredOperatorLogin {
    pub fn into_login(self, url: &str) -> OperatorLogin {
        OperatorLogin {
            url: url.to_owned(),
            token: self.token,
            username: self.username,
            account: self.account,
            expires_at: self.expires_at,
        }
    }
}

/// The operator logins in `~/.mirrord/auth.json`: the part of the login store that every process
/// reaching the operator reads, next to the mirrord Cloud logins the CLI keeps in the same file.
///
/// `mirrord login` and `mirrord logout` write it through the CLI's store; the operator client
/// only reads it, every time it builds a client, the way it reads the kubeconfig. Expired logins
/// stay in the store so that using one fails with a message naming the operator to log in to
/// again, instead of silently falling back to a kubeconfig.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct OperatorLoginStore {
    /// Operator URL (scheme, host and port, no trailing slash) -> login to that operator.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub operators: BTreeMap<String, StoredOperatorLogin>,
    /// Key in [`Self::operators`] of the login mirrord uses. `None` means mirrord uses the
    /// kubeconfig.
    #[serde(
        default,
        rename = "activeOperator",
        skip_serializing_if = "Option::is_none"
    )]
    pub active_operator: Option<String>,
}

/// The login mirrord uses expired. Logging in again is the only way forward: mirrord does not
/// fall back to a kubeconfig while an operator login is selected.
///
/// Implemented by hand because this module builds without the `thiserror` dependency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginExpired {
    pub host: String,
    pub url: String,
}

impl fmt::Display for LoginExpired {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Your login to {} expired. Run mirrord login --url {}.",
            self.host, self.url
        )
    }
}

impl std::error::Error for LoginExpired {}

impl OperatorLoginStore {
    /// `~/.mirrord/auth.json`, or [`None`] when the home directory is unknown.
    ///
    /// Unlike other mirrord documents, the store does not fall back to a relative `~` directory,
    /// which would put credentials under whatever directory `mirrord login` happens to run in.
    pub fn default_path() -> Option<std::path::PathBuf> {
        std::env::home_dir().map(|home| home.join(".mirrord").join("auth.json"))
    }

    /// The stored logins at `path`, empty when the file does not exist. Contents the store cannot
    /// represent read as empty, the way the CLI's store recovers them before its next write.
    #[cfg(feature = "client")]
    pub async fn read(path: &std::path::Path) -> std::io::Result<Self> {
        let contents = match tokio::fs::read(path).await {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(error),
        };
        // A file that exists but does not parse is not "no login": it may hold the login this
        // run should use, and going to the kubeconfig instead would switch identity silently.
        serde_json::from_slice(&contents).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "the file is not a mirrord login store ({error}). Fix or delete it, then run \
                     mirrord login again."
                ),
            )
        })
    }

    /// The login mirrord uses, `None` when it uses the kubeconfig. An active login that expired
    /// at `now` is an error naming the operator, never a silent fallback.
    pub fn active_login(&self, now: DateTime<Utc>) -> Result<Option<OperatorLogin>, LoginExpired> {
        let Some(url) = &self.active_operator else {
            return Ok(None);
        };
        self.login(url, now)
    }

    /// The saved login to the operator at `url`, whether or not it is the selected one: `None`
    /// when there is none, an error when it expired.
    pub fn login(
        &self,
        url: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<OperatorLogin>, LoginExpired> {
        let Some(login) = self.operators.get(url) else {
            return Ok(None);
        };
        let login = login.clone().into_login(url);
        if login.is_valid_at(now).not() {
            return Err(LoginExpired {
                host: login.host().to_owned(),
                url: login.url,
            });
        }
        Ok(Some(login))
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

    fn stored(expires_at: DateTime<Utc>) -> StoredOperatorLogin {
        StoredOperatorLogin {
            token: OperatorLoginToken::from("secret-token".to_owned()),
            username: "dev@corp.example.com".to_owned(),
            account: "dev@corp.example.com".to_owned(),
            expires_at,
            logged_in_at: Utc::now(),
        }
    }

    /// No active operator means the kubeconfig; an active login that expired is an error naming
    /// the operator to log in to again, in the words the developer should run, never a fallback.
    #[test]
    fn active_login_is_the_selected_unexpired_entry_or_an_error() {
        let now = Utc::now();
        let url = "https://mirrord.corp.example.com";
        let mut store = OperatorLoginStore::default();
        assert_eq!(store.active_login(now), Ok(None));

        store
            .operators
            .insert(url.to_owned(), stored(now + TimeDelta::hours(1)));
        assert_eq!(store.active_login(now), Ok(None), "saved but not selected");

        store.active_operator = Some(url.to_owned());
        let login = store.active_login(now).unwrap().unwrap();
        assert_eq!(login.url, url);
        assert_eq!(login.token.expose_secret(), "secret-token");

        let error = store.active_login(now + TimeDelta::hours(2)).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Your login to mirrord.corp.example.com expired. \
             Run mirrord login --url https://mirrord.corp.example.com."
        );

        store.active_operator = Some("https://gone".to_owned());
        assert_eq!(store.active_login(now), Ok(None), "a dangling selection");
    }

    /// The reader shares its shape with the CLI's `~/.mirrord/auth.json`, whose Cloud logins it
    /// skips, and treats a missing or unreadable file as no login.
    #[cfg(feature = "client")]
    #[tokio::test]
    async fn store_reads_the_cli_file_shape() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        assert!(
            OperatorLoginStore::read(&path)
                .await
                .unwrap()
                .active_operator
                .is_none()
        );

        std::fs::write(
            &path,
            r#"{
              "issuers": { "https://app.metalbear.com": { "org": { "token": "t", "email": "a@b", "expiresAt": "2030-01-01T00:00:00Z" } } },
              "operators": { "https://mirrord.corp.example.com": { "token": "secret-token", "username": "dev", "account": "dev@corp.example.com", "expiresAt": "2030-01-01T00:00:00Z", "loggedInAt": "2029-12-31T00:00:00Z" } },
              "activeOperator": "https://mirrord.corp.example.com"
            }"#,
        )
        .unwrap();
        let store = OperatorLoginStore::read(&path).await.unwrap();
        let login = store.active_login(Utc::now()).unwrap().unwrap();
        assert_eq!(login.host(), "mirrord.corp.example.com");

        // A broken file is an error, never a silent switch to the kubeconfig.
        std::fs::write(&path, "not json").unwrap();
        let error = OperatorLoginStore::read(&path).await.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("mirrord login"), "{error}");
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
