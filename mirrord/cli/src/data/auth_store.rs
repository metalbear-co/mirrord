//! Login tokens obtained with `mirrord login`, stored in `~/.mirrord/auth.json`.
//!
//! A mirrord Cloud token is only accepted by session managers that trust the backend that issued
//! it, and it identifies one organization, so the store keeps one token per issuer and
//! organization. Each token's expiry is stored next to it, so expired tokens can be dropped
//! without decoding them.
//!
//! An operator login (`mirrord login --url <operator url>`) is kept per operator URL, with the one
//! mirrord uses named by [`AuthStore::active_operator`]. Logging in to an operator makes it the
//! active one; logging out of the active one hands over to the most recent login that has not
//! expired. Expired operator logins stay in the store so `mirrord whoami` can show them as expired
//! and so using one fails with a message naming the operator to log in to again, instead of
//! silently falling back to a kubeconfig.
//!
//! The file is not encrypted: its security boundary is the OS user account, see
//! [`update_owner_only_at_path`]. Expiry bounds how long a stolen token remains usable.

use std::{
    collections::BTreeMap,
    fmt, io,
    path::{Path, PathBuf},
    sync::LazyLock,
};

use chrono::{DateTime, Utc};
use futures::future::{BoxFuture, FutureExt};
use mirrord_operator::sso::{OperatorLogin, OperatorLoginToken};
use mirrord_sessions_manager_client::{
    LoginToken, LoginTokenSource, SessionsManagerClientError, cloud_url_from_env,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

use super::update_owner_only_at_path;
use crate::{login::issuer, util::mirrord_dir};

/// `~/.mirrord/auth.json`, or [`None`] when the home directory is unknown.
///
/// Unlike other mirrord documents, the store does not fall back to a relative `~` directory, which
/// would put credentials under whatever directory `mirrord login` happens to run in.
static AUTH_STORE_PATH: LazyLock<Option<PathBuf>> =
    LazyLock::new(|| mirrord_dir::get_path().map(|dir| dir.join("auth.json")));

/// Contents of `~/.mirrord/auth.json`.
#[derive(Default, Serialize, Deserialize)]
pub(crate) struct AuthStore {
    /// Issuer (`iss` claim from the token) -> organization ID -> token.
    #[serde(default)]
    issuers: BTreeMap<String, BTreeMap<String, StoredLoginToken>>,
    /// Issuer of the app -> key of the last trial that `mirrord operator signup` started there.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    signups: BTreeMap<String, StoredSignupKey>,
    /// Operator URL (see [`crate::login::issuer`]) -> login to that operator.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    operators: BTreeMap<String, StoredOperatorLogin>,
    /// Key in [`Self::operators`] of the login mirrord uses. `None` means mirrord uses the
    /// kubeconfig.
    #[serde(
        default,
        rename = "activeOperator",
        skip_serializing_if = "Option::is_none"
    )]
    active_operator: Option<String>,
}

/// A login to one operator as stored in [`AuthStore`], keyed by the operator URL.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StoredOperatorLogin {
    pub(crate) token: OperatorLoginToken,
    /// Kubernetes username the operator derived from the identity provider's claims.
    pub(crate) username: String,
    /// The account as the developer knows it, shown by `mirrord whoami`.
    pub(crate) account: String,
    /// When the operator stops accepting the token.
    pub(crate) expires_at: DateTime<Utc>,
    /// When `mirrord login` saved this entry; the latest one takes over when the active login is
    /// removed.
    pub(crate) logged_in_at: DateTime<Utc>,
}

impl StoredOperatorLogin {
    fn into_login(self, url: &str) -> OperatorLogin {
        OperatorLogin {
            url: url.to_owned(),
            token: self.token,
            username: self.username,
            account: self.account,
            expires_at: self.expires_at,
        }
    }
}

/// Every operator login in the store, for `mirrord whoami`.
#[derive(Debug, Default)]
pub(crate) struct OperatorLogins {
    /// Active login first, then the others from the most recent login to the oldest.
    pub(crate) logins: Vec<OperatorLogin>,
    /// URL of the active login, when there is one.
    pub(crate) active: Option<String>,
}

/// What `mirrord logout --url` did to the operator logins.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LogoutOutcome {
    /// No login to that operator was saved.
    NotLoggedIn,
    /// The login was removed. `now_active` is the login mirrord uses afterwards, which is the one
    /// that was active before when the removed login was not the active one.
    Removed {
        was_active: bool,
        now_active: Option<OperatorLogin>,
    },
}

/// Why the active operator login cannot be used.
#[derive(Debug, Error)]
pub(crate) enum ActiveLoginError {
    /// The login mirrord uses expired. Logging in again is the only way forward: mirrord does not
    /// fall back to a kubeconfig while an operator login is selected.
    #[error("Your login to {host} expired. Run mirrord login --url {url}.")]
    Expired { host: String, url: String },
    #[error("failed to read the login store: {0}")]
    #[expect(
        dead_code,
        reason = "constructed by `active_operator_login`, which the commands that connect through a login call"
    )]
    Store(#[source] io::Error),
}

/// A login token as stored in [`AuthStore`].
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StoredLoginToken {
    pub(crate) token: LoginToken,
    /// Email of the user the token identifies, shown to the user.
    pub(crate) email: String,
    /// The token's `exp` claim.
    pub(crate) expires_at: DateTime<Utc>,
}

/// A key of a trial that `mirrord operator signup` started, as stored in [`AuthStore`].
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredSignupKey {
    key: SignupApiKey,
    /// When the trial ends, after which the key is dropped from the store.
    expires_at: DateTime<Utc>,
}

/// The operator API key of a trial started by `mirrord operator signup`, which [`fmt::Debug`] does
/// not print.
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct SignupApiKey(String);

impl SignupApiKey {
    pub(crate) fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Debug for SignupApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// The developer's latest login to the MetalBear cloud set by `MIRRORD_METALBEAR_CLOUD_URL`,
/// which a standalone sessions-manager trusts, read from the store at every lookup.
pub(crate) struct StoredLogin;

impl StoredLogin {
    /// The cloud set by `MIRRORD_METALBEAR_CLOUD_URL`, parsed so its [`issuer`] matches the one
    /// `mirrord login` stored its token under.
    pub(crate) fn cloud_url() -> Result<Url, SessionsManagerClientError> {
        Ok(Url::parse(&cloud_url_from_env()?)?)
    }
}

impl LoginTokenSource for StoredLogin {
    /// A store that cannot be read means no token, rather than failing every request, since a
    /// sessions-manager that does not authenticate its callers does not need one.
    fn login_token(&self) -> BoxFuture<'_, Result<Option<LoginToken>, SessionsManagerClientError>> {
        async {
            let cloud_url = Self::cloud_url()?;
            let login = AuthStore::login_token(issuer(&cloud_url))
                .await
                .unwrap_or_else(|error| {
                    tracing::warn!(%error, "Failed to read the login token store, continuing without a login token");
                    None
                });

            Ok(login.map(|login| login.token))
        }
        .boxed()
    }
}

impl AuthStore {
    /// Stores `token` as the login for `organization_id` with `issuer`, replacing the previous
    /// one.
    pub(crate) async fn save(
        issuer: String,
        organization_id: String,
        token: StoredLoginToken,
    ) -> io::Result<()> {
        Self::save_at(Self::path()?, issuer, organization_id, token, Utc::now()).await
    }

    /// The unexpired login token issued by `issuer` that expires last, if there is one.
    ///
    /// A user in several organizations has a token for each, and the one that expires last is the
    /// one they logged in with most recently.
    pub(crate) async fn login_token(issuer: &str) -> io::Result<Option<StoredLoginToken>> {
        let now = Utc::now();
        Ok(Self::latest_login(issuer)
            .await?
            .filter(|token| token.expires_at > now))
    }

    /// Like [`Self::login_token`], but expired or not, so the user can be told when it expired.
    pub(crate) async fn latest_login(issuer: &str) -> io::Result<Option<StoredLoginToken>> {
        Ok(Self::load()
            .await?
            .and_then(|store| store.latest_token(issuer)))
    }

    /// Stores `key` as the key of the trial started with the app of `issuer`, replacing the
    /// previous one.
    pub(crate) async fn save_signup_key(
        issuer: String,
        key: SignupApiKey,
        expires_at: DateTime<Utc>,
    ) -> io::Result<()> {
        let now = Utc::now();
        update_owner_only_at_path(Self::path()?, move |store: &mut Self| {
            store
                .signups
                .insert(issuer, StoredSignupKey { key, expires_at });
            store.signups.retain(|_, stored| stored.expires_at > now);
            Ok::<_, io::Error>(())
        })
        .await?;

        Ok(())
    }

    /// The key of the unexpired trial started with the app of `issuer`, if there is one.
    pub(crate) async fn signup_key(issuer: &str) -> io::Result<Option<SignupApiKey>> {
        let now = Utc::now();
        Ok(Self::load()
            .await?
            .and_then(|mut store| store.signups.remove(issuer))
            .filter(|stored| stored.expires_at > now)
            .map(|stored| stored.key))
    }

    /// Stores `login` as the login to the operator at `url` and makes it the one mirrord uses.
    pub(crate) async fn save_operator_login(
        url: String,
        login: StoredOperatorLogin,
    ) -> io::Result<()> {
        update_owner_only_at_path(Self::path()?, move |store: &mut Self| {
            store.insert_operator(url, login);
            Ok::<_, io::Error>(())
        })
        .await?;

        Ok(())
    }

    /// Makes the saved login to the operator at `url` the one mirrord uses, when it has not
    /// expired. `None` when there is no such login, so the caller logs in through the browser.
    pub(crate) async fn activate_operator_login(url: String) -> io::Result<Option<OperatorLogin>> {
        let now = Utc::now();
        let store = update_owner_only_at_path(Self::path()?, move |store: &mut Self| {
            store.activate_operator(&url, now);
            Ok::<_, io::Error>(())
        })
        .await?;

        Ok(store.active_login(now).ok().flatten())
    }

    /// Removes the login to the operator at `url`, handing the active role over when it had it.
    pub(crate) async fn remove_operator_login(url: String) -> io::Result<LogoutOutcome> {
        let now = Utc::now();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        update_owner_only_at_path(Self::path()?, move |store: &mut Self| {
            let _ = sender.send(store.remove_operator(&url, now));
            Ok::<_, io::Error>(())
        })
        .await?;

        receiver
            .await
            .map_err(|_| io::Error::other("the login store update did not report its outcome"))
    }

    /// Removes every mirrord Cloud token issued by `issuer`. `false` when there was none.
    pub(crate) async fn remove_cloud_login(issuer: &str) -> io::Result<bool> {
        let issuer = issuer.to_owned();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        update_owner_only_at_path(Self::path()?, move |store: &mut Self| {
            let _ = sender.send(store.issuers.remove(&issuer).is_some());
            Ok::<_, io::Error>(())
        })
        .await?;

        receiver
            .await
            .map_err(|_| io::Error::other("the login store update did not report its outcome"))
    }

    /// Every operator login, for `mirrord whoami`.
    pub(crate) async fn operator_logins() -> io::Result<OperatorLogins> {
        Ok(Self::load()
            .await?
            .map(Self::into_operator_logins)
            .unwrap_or_default())
    }

    /// The operator login mirrord uses, `None` when it uses the kubeconfig. An expired active
    /// login is an error naming the operator, never a silent fallback.
    ///
    /// Nothing calls it yet: the commands that connect to the operator through a login instead of a
    /// kubeconfig start here (INT-1040).
    #[expect(
        dead_code,
        reason = "entry point for the commands that connect through a login (INT-1040)"
    )]
    pub(crate) async fn active_operator_login() -> Result<Option<OperatorLogin>, ActiveLoginError> {
        let Some(store) = Self::load().await.map_err(ActiveLoginError::Store)? else {
            return Ok(None);
        };
        store.active_login(Utc::now())
    }

    fn insert_operator(&mut self, url: String, login: StoredOperatorLogin) {
        self.operators.insert(url.clone(), login);
        self.active_operator = Some(url);
    }

    /// Selects the login to `url` when it exists and is valid at `now`.
    fn activate_operator(&mut self, url: &str, now: DateTime<Utc>) -> bool {
        let valid = self
            .operators
            .get(url)
            .is_some_and(|login| login.expires_at > now);
        if valid {
            self.active_operator = Some(url.to_owned());
        }
        valid
    }

    /// Removes the login to `url`. When it was the active one, the most recent login that is
    /// valid at `now` takes over, or none does and mirrord goes back to the kubeconfig.
    fn remove_operator(&mut self, url: &str, now: DateTime<Utc>) -> LogoutOutcome {
        if self.operators.remove(url).is_none() {
            return LogoutOutcome::NotLoggedIn;
        }
        let was_active = self.active_operator.as_deref() == Some(url);
        if was_active {
            self.active_operator = self
                .operators
                .iter()
                .filter(|(_, login)| login.expires_at > now)
                .max_by_key(|(_, login)| login.logged_in_at)
                .map(|(url, _)| url.clone());
        }
        let now_active = self
            .active_operator
            .as_ref()
            .and_then(|active| self.operators.get(active).map(|login| (active, login)))
            .map(|(url, login)| login.clone().into_login(url));
        LogoutOutcome::Removed {
            was_active,
            now_active,
        }
    }

    fn active_login(&self, now: DateTime<Utc>) -> Result<Option<OperatorLogin>, ActiveLoginError> {
        let Some(url) = &self.active_operator else {
            return Ok(None);
        };
        let Some(login) = self.operators.get(url) else {
            return Ok(None);
        };
        let login = login.clone().into_login(url);
        if !login.is_valid_at(now) {
            return Err(ActiveLoginError::Expired {
                host: login.host().to_owned(),
                url: login.url,
            });
        }
        Ok(Some(login))
    }

    fn into_operator_logins(self) -> OperatorLogins {
        let active = self.active_operator;
        let mut entries = self.operators.into_iter().collect::<Vec<_>>();
        entries.sort_by(|(url_a, a), (url_b, b)| {
            let is_active = |url: &String| active.as_ref() == Some(url);
            is_active(url_b)
                .cmp(&is_active(url_a))
                .then_with(|| b.logged_in_at.cmp(&a.logged_in_at))
        });
        OperatorLogins {
            logins: entries
                .into_iter()
                .map(|(url, login)| login.into_login(&url))
                .collect(),
            active,
        }
    }

    fn path() -> io::Result<&'static Path> {
        AUTH_STORE_PATH.as_deref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "could not determine the home directory",
            )
        })
    }

    /// The stored contents, or [`None`] when nothing is stored.
    async fn load() -> io::Result<Option<Self>> {
        let Some(path) = AUTH_STORE_PATH.as_deref() else {
            return Ok(None);
        };

        match tokio::fs::read(path).await {
            Ok(contents) => Ok(Some(serde_json::from_slice(&contents).unwrap_or_default())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn latest_token(mut self, issuer: &str) -> Option<StoredLoginToken> {
        self.issuers
            .remove(issuer)?
            .into_values()
            .max_by_key(|token| token.expires_at)
    }

    async fn save_at(
        path: &Path,
        issuer: String,
        organization_id: String,
        token: StoredLoginToken,
        now: DateTime<Utc>,
    ) -> io::Result<()> {
        update_owner_only_at_path(path, move |store: &mut Self| {
            store.insert(issuer, organization_id, token, now);
            Ok::<_, io::Error>(())
        })
        .await?;

        Ok(())
    }

    /// Inserts `token` and drops every token that is expired at `now`.
    fn insert(
        &mut self,
        issuer: String,
        organization_id: String,
        token: StoredLoginToken,
        now: DateTime<Utc>,
    ) {
        self.issuers
            .entry(issuer)
            .or_default()
            .insert(organization_id, token);

        self.issuers.retain(|_, organizations| {
            organizations.retain(|_, token| token.expires_at > now);
            !organizations.is_empty()
        });
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use chrono::TimeDelta;
    #[cfg(unix)]
    use tempfile::tempdir;

    use super::*;

    const ISSUER: &str = "https://app.metalbear.com";

    fn token(value: &str, expires_at: DateTime<Utc>) -> StoredLoginToken {
        StoredLoginToken {
            token: LoginToken::from(value.to_owned()),
            email: "kasia@example.com".to_owned(),
            expires_at,
        }
    }

    fn stored_tokens(store: &AuthStore) -> Vec<(&str, &str, &str)> {
        store
            .issuers
            .iter()
            .flat_map(|(issuer, organizations)| {
                organizations.iter().map(|(organization, token)| {
                    (
                        issuer.as_str(),
                        organization.as_str(),
                        token.token.expose_secret(),
                    )
                })
            })
            .collect()
    }

    #[test]
    fn tokens_are_kept_per_issuer_and_organization() {
        let now = Utc::now();
        let later = now + TimeDelta::hours(24);
        let mut store = AuthStore::default();

        store.insert(
            ISSUER.to_owned(),
            "org-a".to_owned(),
            token("a1", later),
            now,
        );
        store.insert(
            ISSUER.to_owned(),
            "org-b".to_owned(),
            token("b", later),
            now,
        );
        store.insert(
            "https://other".to_owned(),
            "org-a".to_owned(),
            token("x", later),
            now,
        );
        store.insert(
            ISSUER.to_owned(),
            "org-a".to_owned(),
            token("a2", later),
            now,
        );

        assert_eq!(
            stored_tokens(&store),
            [
                (ISSUER, "org-a", "a2"),
                (ISSUER, "org-b", "b"),
                ("https://other", "org-a", "x"),
            ]
        );
    }

    #[test]
    fn expired_tokens_are_dropped() {
        let now = Utc::now();
        let mut store = AuthStore::default();

        store.insert(
            "https://other".to_owned(),
            "org-a".to_owned(),
            token("expired", now - TimeDelta::seconds(1)),
            now - TimeDelta::hours(24),
        );
        store.insert(
            ISSUER.to_owned(),
            "org-a".to_owned(),
            token("valid", now + TimeDelta::hours(24)),
            now,
        );

        assert_eq!(stored_tokens(&store), [(ISSUER, "org-a", "valid")]);
    }

    /// The latest login is the one that expires last, whichever organization it is for. It is
    /// returned even once expired, so the CLI can say when it expired.
    #[test]
    fn latest_login_is_the_one_that_expires_last_at_the_issuer() {
        let now = Utc::now();
        let mut store = AuthStore::default();
        for (issuer, organization, value, expires_at) in [
            (ISSUER, "org-a", "older", now + TimeDelta::hours(1)),
            (ISSUER, "org-b", "newer", now + TimeDelta::hours(2)),
            ("https://other", "org-a", "other", now + TimeDelta::hours(3)),
        ] {
            store.insert(
                issuer.to_owned(),
                organization.to_owned(),
                token(value, expires_at),
                now,
            );
        }

        let latest = store.latest_token(ISSUER).unwrap();
        assert_eq!(latest.token.expose_secret(), "newer");

        let mut store = AuthStore::default();
        store.insert(
            ISSUER.to_owned(),
            "org-a".to_owned(),
            token("expired", now - TimeDelta::seconds(1)),
            now - TimeDelta::hours(24),
        );
        assert_eq!(
            store.latest_token(ISSUER).unwrap().token.expose_secret(),
            "expired"
        );
        assert!(AuthStore::default().latest_token(ISSUER).is_none());
    }

    /// Spellings of one backend that `Url` normalizes share an issuer, so a token stored by
    /// `mirrord login` is found whichever spelling `MIRRORD_METALBEAR_CLOUD_URL` uses.
    #[test]
    fn spellings_of_one_backend_share_an_issuer() {
        for spelling in [
            "https://app.metalbear.com",
            "https://app.metalbear.com/",
            "https://APP.MetalBear.com",
            "https://app.metalbear.com:443",
        ] {
            assert_eq!(issuer(&Url::parse(spelling).unwrap()), ISSUER, "{spelling}");
        }
    }

    #[test]
    fn debug_output_does_not_contain_the_token() {
        let token = token("secret-token", Utc::now());

        assert!(!format!("{token:?}").contains("secret-token"));
    }

    const OPERATOR: &str = "https://mirrord.corp.example.com";
    const STAGING: &str = "https://staging.mirrord.corp.example.com";

    fn operator_login(
        value: &str,
        expires_at: DateTime<Utc>,
        logged_in_at: DateTime<Utc>,
    ) -> StoredOperatorLogin {
        StoredOperatorLogin {
            token: OperatorLoginToken::from(value.to_owned()),
            username: "dev@corp.example.com".to_owned(),
            account: "dev@corp.example.com".to_owned(),
            expires_at,
            logged_in_at,
        }
    }

    /// The last operator logged in to is the one mirrord uses, whatever was active before.
    #[test]
    fn logging_in_to_an_operator_makes_it_active() {
        let now = Utc::now();
        let mut store = AuthStore::default();

        store.insert_operator(
            OPERATOR.to_owned(),
            operator_login("a", now + TimeDelta::hours(24), now),
        );
        assert_eq!(store.active_operator.as_deref(), Some(OPERATOR));

        store.insert_operator(
            STAGING.to_owned(),
            operator_login("b", now + TimeDelta::hours(24), now + TimeDelta::minutes(1)),
        );
        assert_eq!(store.active_operator.as_deref(), Some(STAGING));
        assert_eq!(store.operators.len(), 2);

        // A Cloud login lives in its own map and leaves the operator in use alone.
        store.insert(
            ISSUER.to_owned(),
            "org-a".to_owned(),
            token("c", now + TimeDelta::hours(24)),
            now,
        );
        assert_eq!(store.active_operator.as_deref(), Some(STAGING));
    }

    /// Switching to a saved login needs no browser, but only while the login is valid.
    #[test]
    fn activating_a_saved_login_requires_it_to_be_valid() {
        let now = Utc::now();
        let mut store = AuthStore::default();
        store.insert_operator(
            OPERATOR.to_owned(),
            operator_login("a", now + TimeDelta::hours(1), now),
        );
        store.insert_operator(
            STAGING.to_owned(),
            operator_login("b", now - TimeDelta::seconds(1), now - TimeDelta::hours(24)),
        );
        store.active_operator = None;

        assert!(store.activate_operator(OPERATOR, now));
        assert_eq!(store.active_operator.as_deref(), Some(OPERATOR));
        assert!(!store.activate_operator(STAGING, now));
        assert!(!store.activate_operator("https://unknown", now));
        assert_eq!(store.active_operator.as_deref(), Some(OPERATOR));
    }

    /// Logging out of the active login hands over to the most recent login that has not
    /// expired; with none left, mirrord goes back to the kubeconfig.
    #[test]
    fn logging_out_of_the_active_login_hands_over_to_the_latest_valid_one() {
        let now = Utc::now();
        let mut store = AuthStore::default();
        store.insert_operator(
            "https://old".to_owned(),
            operator_login("old", now + TimeDelta::hours(1), now - TimeDelta::hours(3)),
        );
        store.insert_operator(
            "https://expired".to_owned(),
            operator_login(
                "expired",
                now - TimeDelta::seconds(1),
                now - TimeDelta::hours(1),
            ),
        );
        store.insert_operator(
            STAGING.to_owned(),
            operator_login(
                "newer",
                now + TimeDelta::hours(1),
                now - TimeDelta::hours(2),
            ),
        );
        store.insert_operator(
            OPERATOR.to_owned(),
            operator_login("active", now + TimeDelta::hours(1), now),
        );

        // Logging out of a login that is not active changes nothing about the active one.
        let outcome = store.remove_operator("https://old", now);
        let LogoutOutcome::Removed {
            was_active: false,
            now_active: Some(now_active),
        } = outcome
        else {
            panic!("{outcome:?}");
        };
        assert_eq!(now_active.url, OPERATOR);

        // The most recent valid login takes over, skipping the expired one logged in to later.
        let outcome = store.remove_operator(OPERATOR, now);
        let LogoutOutcome::Removed {
            was_active: true,
            now_active: Some(now_active),
        } = outcome
        else {
            panic!("{outcome:?}");
        };
        assert_eq!(now_active.url, STAGING);
        assert_eq!(store.active_operator.as_deref(), Some(STAGING));

        assert_eq!(
            store.remove_operator(STAGING, now),
            LogoutOutcome::Removed {
                was_active: true,
                now_active: None,
            }
        );
        assert_eq!(store.active_operator, None);
        assert_eq!(
            store.remove_operator(STAGING, now),
            LogoutOutcome::NotLoggedIn
        );
    }

    /// An expired active login is reported with the operator to log in to again, in the words the
    /// developer should run.
    #[test]
    fn expired_active_login_names_the_operator_to_log_in_to() {
        let now = Utc::now();
        let mut store = AuthStore::default();
        assert!(store.active_login(now).unwrap().is_none());

        store.insert_operator(
            OPERATOR.to_owned(),
            operator_login("a", now + TimeDelta::hours(1), now),
        );
        assert_eq!(store.active_login(now).unwrap().unwrap().url, OPERATOR);

        let error = store.active_login(now + TimeDelta::hours(2)).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Your login to mirrord.corp.example.com expired. \
             Run mirrord login --url https://mirrord.corp.example.com."
        );
    }

    /// `mirrord whoami` lists the active login first, then the others newest first.
    #[test]
    fn operator_logins_list_the_active_one_first() {
        let now = Utc::now();
        let mut store = AuthStore::default();
        store.insert_operator(
            STAGING.to_owned(),
            operator_login(
                "staging",
                now + TimeDelta::hours(1),
                now - TimeDelta::hours(1),
            ),
        );
        store.insert_operator(
            "https://old".to_owned(),
            operator_login("old", now + TimeDelta::hours(1), now - TimeDelta::hours(2)),
        );
        store.insert_operator(
            OPERATOR.to_owned(),
            operator_login("active", now + TimeDelta::hours(1), now),
        );
        store.active_operator = Some(STAGING.to_owned());

        let logins = store.into_operator_logins();
        assert_eq!(logins.active.as_deref(), Some(STAGING));
        assert_eq!(
            logins
                .logins
                .iter()
                .map(|login| login.url.as_str())
                .collect::<Vec<_>>(),
            [STAGING, OPERATOR, "https://old"]
        );
    }

    /// A store written before operator logins existed still loads, and one with operator logins
    /// keeps the Cloud tokens untouched.
    #[test]
    fn store_shape_is_backwards_compatible() {
        let legacy = serde_json::json!({
            "issuers": { ISSUER: { "org-a": { "token": "t", "email": "a@b", "expiresAt": "2030-01-01T00:00:00Z" } } }
        });
        let store: AuthStore = serde_json::from_value(legacy).unwrap();
        assert!(store.operators.is_empty());
        assert_eq!(store.active_operator, None);

        let now = Utc::now();
        let mut store = store;
        store.insert_operator(
            OPERATOR.to_owned(),
            operator_login("a", now + TimeDelta::hours(1), now),
        );
        let json = serde_json::to_value(&store).unwrap();
        let at = |path: &[&str]| {
            path.iter()
                .try_fold(&json, |value, key| value.get(key))
                .cloned()
                .unwrap_or_default()
        };
        assert_eq!(at(&["activeOperator"]), OPERATOR);
        assert_eq!(at(&["operators", OPERATOR, "token"]), "a");
        assert_eq!(at(&["issuers", ISSUER, "org-a", "token"]), "t");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn store_is_accessible_only_to_the_owner() {
        let home = tempdir().unwrap();
        let directory = home.path().join(".mirrord");
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = directory.join("auth.json");
        let lock_path = path.with_extension("lock");
        for file in [&path, &lock_path] {
            std::fs::write(file, "{}").unwrap();
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o644)).unwrap();
        }

        let now = Utc::now();
        AuthStore::save_at(
            &path,
            ISSUER.to_owned(),
            "org-a".to_owned(),
            token("a", now + TimeDelta::hours(24)),
            now,
        )
        .await
        .unwrap();

        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&directory), 0o700);
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&lock_path), 0o600);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unchanged_store_is_restricted_to_the_owner() {
        let home = tempdir().unwrap();
        let path = home.path().join(".mirrord").join("auth.json");
        let now = Utc::now();
        let save = || {
            AuthStore::save_at(
                &path,
                ISSUER.to_owned(),
                "org-a".to_owned(),
                token("a", now + TimeDelta::hours(24)),
                now,
            )
        };

        save().await.unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let contents = std::fs::read(&path).unwrap();
        save().await.unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), contents);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
