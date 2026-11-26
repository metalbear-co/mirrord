//! How a process reaches the operator: through the kubeconfig, or through the operator login that
//! `mirrord login --url` saved and selected.
//!
//! The login store is read every time a client is built, the way the kubeconfig is, so the CLI,
//! the internal proxy and every other process of a run agree on the operator without passing a
//! token between them. The checks that need the resolved config, such as `operator: false` or a
//! `kube_context` that the login makes meaningless, live here as pure functions and run once, when
//! the CLI resolves its config.

use std::io;

use chrono::Utc;
use mirrord_config::LayerConfig;
use thiserror::Error;

use crate::sso::{LoginExpired, OperatorLogin, OperatorLoginStore, host_of};

/// Which credentials a client to the operator carries.
#[derive(Debug, Clone)]
pub enum OperatorAccess {
    /// The kubeconfig's credentials, through kube-apiserver. `context` is the kubeconfig context
    /// that was selected, when one was.
    Kubeconfig { context: Option<String> },
    /// The saved operator login, as a bearer token on the operator's public URL. The kubeconfig is
    /// not read.
    Login(OperatorLogin),
}

impl OperatorAccess {
    /// The kubeconfig context the client was built from, `None` under a login.
    pub fn kube_context(&self) -> Option<&str> {
        match self {
            Self::Kubeconfig { context } => context.as_deref(),
            Self::Login(..) => None,
        }
    }

    pub fn login(&self) -> Option<&OperatorLogin> {
        match self {
            Self::Kubeconfig { .. } => None,
            Self::Login(login) => Some(login),
        }
    }
}

/// Why the saved operator login cannot be used for this run.
#[derive(Debug, Error)]
pub enum OperatorAccessError {
    #[error(transparent)]
    Expired(#[from] LoginExpired),

    /// The config turns the operator off while a login, which only the operator accepts, is
    /// selected. The two cannot both be meant, so neither wins silently.
    #[error(
        "operator: false can't be used while logged in to {host}. Run mirrord logout --url {url} first."
    )]
    OperatorDisabled { host: String, url: String },

    /// The session was prepared under a login that is no longer in the store, so there is no
    /// token to reconnect with. The login selected since does not stand in: its operator knows
    /// nothing of this session.
    #[error(
        "this session started under a login to {host} that was removed since. Run mirrord login --url {url} to let it reconnect."
    )]
    SessionLoginRemoved { host: String, url: String },

    #[error("failed to read the login store at {path}: {error}")]
    Store {
        path: String,
        #[source]
        error: io::Error,
    },

    /// The URL came from the developer at login time, so an unparsable one is reported, not a
    /// panic.
    #[error("the saved login URL {url} is not a valid URL: {error}")]
    InvalidUrl {
        url: String,
        #[source]
        error: http::uri::InvalidUri,
    },
}

/// The operator login mirrord uses, read from `~/.mirrord/auth.json`. `None` when mirrord uses the
/// kubeconfig, which is also the answer when the home directory is unknown: nothing could have
/// been saved there.
pub async fn active_operator_login() -> Result<Option<OperatorLogin>, OperatorAccessError> {
    Ok(read_login_store().await?.active_login(Utc::now())?)
}

/// The login a session reconnects with: the one to the operator the session was prepared on,
/// read from `~/.mirrord/auth.json` by that operator's `url`, whatever login is selected now.
/// A `mirrord login --url` to another operator in the meantime must not send this session there.
pub async fn session_login(url: &str) -> Result<OperatorLogin, OperatorAccessError> {
    session_login_in(&read_login_store().await?, url, Utc::now())
}

/// Pure half of [`session_login`], for the test below.
pub fn session_login_in(
    store: &OperatorLoginStore,
    url: &str,
    now: chrono::DateTime<Utc>,
) -> Result<OperatorLogin, OperatorAccessError> {
    store
        .login(url, now)?
        .ok_or_else(|| OperatorAccessError::SessionLoginRemoved {
            host: host_of(url).to_owned(),
            url: url.to_owned(),
        })
}

/// `~/.mirrord/auth.json`, or an empty store when the home directory is unknown: nothing could
/// have been saved there.
async fn read_login_store() -> Result<OperatorLoginStore, OperatorAccessError> {
    let Some(path) = OperatorLoginStore::default_path() else {
        return Ok(OperatorLoginStore::default());
    };
    OperatorLoginStore::read(&path)
        .await
        .map_err(|error| OperatorAccessError::Store {
            path: path.display().to_string(),
            error,
        })
}

/// Refuses `operator: false` under a login: without the operator there is nothing the login can
/// reach. Runs on its own as well, on the config after the global defaults are merged in, since a
/// global `operator: false` only shows up there.
pub fn check_operator_enabled_under_login(
    config: &LayerConfig,
    login: &OperatorLogin,
) -> Result<(), OperatorAccessError> {
    if config.operator == Some(false) {
        return Err(OperatorAccessError::OperatorDisabled {
            host: login.host().to_owned(),
            url: login.url.clone(),
        });
    }
    Ok(())
}

/// Checks the config of a run against the operator login it runs under, and returns the warnings
/// to show for settings the login makes meaningless, each naming the setting and the operator.
///
/// `operator: false` is an error: a login only works through the operator. `kubeconfig` and
/// `kube_context` are ignored, since no kubeconfig is read while logged in.
pub fn check_config_under_login(
    config: &LayerConfig,
    login: &OperatorLogin,
) -> Result<Vec<String>, OperatorAccessError> {
    check_operator_enabled_under_login(config, login)?;

    let ignored = [
        ("kubeconfig", config.kubeconfig.is_some()),
        ("kube_context", config.kube_context.is_some()),
    ];
    Ok(ignored
        .into_iter()
        .filter(|(_, set)| *set)
        .map(|(setting, _)| format!("{setting} is ignored while logged in to {}", login.host()))
        .collect())
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;
    use mirrord_config::{
        LayerFileConfig,
        config::{ConfigContext, MirrordConfig},
    };

    use super::*;
    use crate::sso::{OperatorLoginToken, StoredOperatorLogin};

    fn login() -> OperatorLogin {
        OperatorLogin {
            url: "https://mirrord.corp.example.com".to_owned(),
            token: OperatorLoginToken::from("secret-token".to_owned()),
            username: "dev@corp.example.com".to_owned(),
            account: "dev@corp.example.com".to_owned(),
            expires_at: Utc::now() + TimeDelta::hours(1),
        }
    }

    fn config() -> LayerConfig {
        let mut context = ConfigContext::default().strict_env(true);
        LayerFileConfig::default()
            .generate_config(&mut context)
            .expect("the default config generates")
    }

    /// A config that says nothing about the cluster has nothing to warn about under a login.
    #[test]
    fn plain_config_passes_without_warnings() {
        let warnings = check_config_under_login(&config(), &login()).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");

        let mut config = config();
        config.operator = Some(true);
        assert!(
            check_config_under_login(&config, &login())
                .unwrap()
                .is_empty()
        );
    }

    /// `operator: false` and a selected login contradict each other, so the run stops and says
    /// which login to end.
    #[test]
    fn operator_false_is_refused_with_the_logout_command() {
        let mut config = config();
        config.operator = Some(false);
        let error = check_config_under_login(&config, &login()).unwrap_err();
        assert_eq!(
            error.to_string(),
            "operator: false can't be used while logged in to mirrord.corp.example.com. \
             Run mirrord logout --url https://mirrord.corp.example.com first."
        );
    }

    /// Each cluster-picking setting gets its own warning, naming the setting and the operator.
    #[test]
    fn kubeconfig_and_kube_context_are_ignored_with_a_warning_each() {
        let mut config = config();
        config.kube_context = Some("staging-eu".to_owned());
        assert_eq!(
            check_config_under_login(&config, &login()).unwrap(),
            ["kube_context is ignored while logged in to mirrord.corp.example.com"]
        );

        config.kubeconfig = Some("~/.kube/other".to_owned());
        assert_eq!(
            check_config_under_login(&config, &login()).unwrap(),
            [
                "kubeconfig is ignored while logged in to mirrord.corp.example.com",
                "kube_context is ignored while logged in to mirrord.corp.example.com",
            ]
        );
    }

    /// An expired login surfaces through the same error type, so one match reports every reason
    /// the login cannot be used.
    #[test]
    fn expired_login_keeps_its_message() {
        let error = OperatorAccessError::from(LoginExpired {
            host: "mirrord.corp.example.com".to_owned(),
            url: "https://mirrord.corp.example.com".to_owned(),
        });
        assert_eq!(
            error.to_string(),
            "Your login to mirrord.corp.example.com expired. \
             Run mirrord login --url https://mirrord.corp.example.com."
        );
    }

    /// A session prepared on operator A reconnects to A after `mirrord login --url` selected
    /// operator B. Before, the reconnect reread the selected login and sent A's session to B,
    /// which knows nothing of it, so the reconnect failed.
    #[test]
    fn session_login_is_the_sessions_operator_not_the_selected_one() {
        let now = Utc::now();
        let stored = |token: &str| StoredOperatorLogin {
            token: OperatorLoginToken::from(token.to_owned()),
            username: "dev@corp.example.com".to_owned(),
            account: "dev@corp.example.com".to_owned(),
            expires_at: now + TimeDelta::hours(1),
            logged_in_at: now,
        };
        let a = "https://a.mirrord.corp.example.com";
        let b = "https://b.mirrord.corp.example.com";
        let mut store = OperatorLoginStore::default();
        store.operators.insert(a.to_owned(), stored("token-a"));
        store.operators.insert(b.to_owned(), stored("token-b"));
        store.active_operator = Some(b.to_owned());

        let login = session_login_in(&store, a, now).unwrap();
        assert_eq!(login.url, a);
        assert_eq!(login.token.expose_secret(), "token-a");

        store.operators.remove(a);
        assert_eq!(
            session_login_in(&store, a, now).unwrap_err().to_string(),
            "this session started under a login to a.mirrord.corp.example.com that was removed \
             since. Run mirrord login --url https://a.mirrord.corp.example.com to let it \
             reconnect."
        );
    }
}
