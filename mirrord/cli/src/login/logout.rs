//! `mirrord logout --url`: removes a login and says what mirrord uses afterwards.

use mirrord_kube::api::kubernetes::resolve_kube_context;
use mirrord_operator::sso::OperatorLogin;
use mirrord_sessions_manager_client::cloud_url_from_env;
use url::Url;

use super::{LoginError, backend_base_url, host_of, issuer};
use crate::{
    config::LogoutArgs,
    data::{AuthStore, LogoutOutcome},
};

/// Implementation of the `mirrord logout` command.
pub(crate) async fn logout_command(args: LogoutArgs) -> Result<(), LoginError> {
    let url = backend_base_url(args.url)?;
    let host = host_of(&url);

    let cloud_url = cloud_url_from_env()
        .ok()
        .and_then(|cloud_url| Url::parse(&cloud_url).ok());
    if cloud_url.is_some_and(|cloud_url| issuer(&cloud_url) == issuer(&url)) {
        if !AuthStore::remove_cloud_login(issuer(&url))
            .await
            .map_err(LoginError::Store)?
        {
            return Err(LoginError::NotLoggedIn(host));
        }
        println!("Logged out of mirrord Cloud ({host}).");
        return Ok(());
    }

    match AuthStore::remove_operator_login(issuer(&url).to_owned())
        .await
        .map_err(LoginError::Store)?
    {
        LogoutOutcome::NotLoggedIn => Err(LoginError::NotLoggedIn(host)),
        LogoutOutcome::Removed {
            was_active,
            now_active,
        } => {
            // Resolved only for the message; a missing kubeconfig is reported, not an error.
            let kube_context = resolve_kube_context(None, None).ok().flatten();
            println!(
                "{}",
                logout_message(
                    &host,
                    was_active,
                    now_active.as_ref(),
                    kube_context.as_deref()
                )
            );
            Ok(())
        }
    }
}

/// What the developer reads after a logout: which login was removed and what mirrord uses now,
/// so a session started next connects where they expect.
fn logout_message(
    host: &str,
    was_active: bool,
    now_active: Option<&OperatorLogin>,
    kube_context: Option<&str>,
) -> String {
    let uses = match (was_active, now_active, kube_context) {
        (true, Some(login), _) => format!("mirrord uses your login to {} now.", login.host()),
        (false, Some(login), _) => format!("mirrord still uses your login to {}.", login.host()),
        (_, None, Some(context)) => format!("mirrord uses your kubeconfig context {context}."),
        (_, None, None) => "No kubeconfig was found.".to_owned(),
    };
    format!("Logged out of {host}. {uses}")
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use mirrord_operator::sso::OperatorLoginToken;

    use super::*;

    fn login(url: &str) -> OperatorLogin {
        OperatorLogin {
            url: url.to_owned(),
            token: OperatorLoginToken::from("t".to_owned()),
            username: "dev".to_owned(),
            account: "dev".to_owned(),
            expires_at: Utc::now(),
        }
    }

    /// The ticket's example: logging out of the operator in use with a kubeconfig to fall back on.
    #[test]
    fn names_the_kubeconfig_context_after_logging_out_of_the_active_login() {
        assert_eq!(
            logout_message("mirrord.corp.example.com", true, None, Some("staging-eu")),
            "Logged out of mirrord.corp.example.com. mirrord uses your kubeconfig context staging-eu."
        );
    }

    #[test]
    fn names_the_login_mirrord_uses_next() {
        let staging = login("https://staging.mirrord.corp.example.com");
        assert_eq!(
            logout_message(
                "mirrord.corp.example.com",
                true,
                Some(&staging),
                Some("staging-eu")
            ),
            "Logged out of mirrord.corp.example.com. mirrord uses your login to \
             staging.mirrord.corp.example.com now."
        );
        assert_eq!(
            logout_message("mirrord.corp.example.com", false, Some(&staging), None),
            "Logged out of mirrord.corp.example.com. mirrord still uses your login to \
             staging.mirrord.corp.example.com."
        );
        assert_eq!(
            logout_message("mirrord.corp.example.com", true, None, None),
            "Logged out of mirrord.corp.example.com. No kubeconfig was found."
        );
    }
}
