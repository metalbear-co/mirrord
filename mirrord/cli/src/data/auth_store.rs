//! Login tokens obtained with `mirrord login`, stored in `~/.mirrord/auth.json`.
//!
//! A token is only accepted by session managers that trust the backend that issued it, and it
//! identifies one organization, so the store keeps one token per issuer and organization. Each
//! token's expiry is stored next to it, so expired tokens can be dropped without decoding them.
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
use mirrord_sessions_manager_client::{
    LoginToken, LoginTokenSource, SessionsManagerClientError, cloud_url_from_env,
};
use serde::{Deserialize, Serialize};
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

    #[test]
    fn debug_output_does_not_contain_the_token() {
        let token = token("secret-token", Utc::now());

        assert!(!format!("{token:?}").contains("secret-token"));
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
