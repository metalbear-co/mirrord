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
use serde::{Deserialize, Serialize};

use super::{default_path, update_owner_only_at_path};

static AUTH_STORE_PATH: LazyLock<PathBuf> = LazyLock::new(|| default_path("auth.json"));

/// Contents of `~/.mirrord/auth.json`.
#[derive(Default, Serialize, Deserialize)]
pub(crate) struct AuthStore {
    /// Issuer (`iss` claim from the token) -> organization ID -> token.
    #[serde(default)]
    issuers: BTreeMap<String, BTreeMap<String, StoredLoginToken>>,
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

/// A signed login token.
///
/// Tokens must never appear in logs, so [`fmt::Debug`] does not print it.
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct LoginToken(String);

impl From<String> for LoginToken {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl fmt::Debug for LoginToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
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
        Self::save_at(&AUTH_STORE_PATH, issuer, organization_id, token, Utc::now()).await
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
                        token.token.0.as_str(),
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
        std::fs::write(&path, "{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

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
        assert_eq!(mode(&path.with_extension("lock")), 0o600);
    }
}
