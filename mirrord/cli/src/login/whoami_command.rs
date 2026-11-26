//! `mirrord whoami`: which operator mirrord uses, the other operator logins, and the mirrord Cloud
//! login.

use chrono::{DateTime, Utc};
use mirrord_kube::api::kubernetes::resolve_kube_context;

use super::{LoginError, expiry::expires_in, issuer};
use crate::data::{AuthStore, OperatorLogins, StoredLogin};

/// Implementation of the `mirrord whoami` command.
pub(crate) async fn whoami_command() -> Result<(), LoginError> {
    let operators = AuthStore::operator_logins()
        .await
        .map_err(LoginError::Store)?;
    let cloud = match StoredLogin::cloud_url() {
        Ok(cloud_url) => AuthStore::latest_login(issuer(&cloud_url))
            .await
            .map_err(LoginError::Store)?
            .map(|login| CloudLogin {
                email: login.email,
                expires_at: login.expires_at,
            }),
        Err(error) => {
            tracing::warn!(%error, "Could not read the mirrord Cloud URL, skipping the Cloud login");
            None
        }
    };
    // Resolved only to tell the developer what mirrord falls back to; a missing or broken
    // kubeconfig is a fact to report, not an error of this command.
    let kube_context = resolve_kube_context(None, None).ok().flatten();

    println!(
        "{}",
        render(
            &operators,
            cloud.as_ref(),
            kube_context.as_deref(),
            Utc::now()
        )
    );
    Ok(())
}

/// The latest mirrord Cloud login, expired or not.
struct CloudLogin {
    email: String,
    expires_at: DateTime<Utc>,
}

/// The whole output, built from the store and the clock so it can be checked word for word.
fn render(
    operators: &OperatorLogins,
    cloud: Option<&CloudLogin>,
    kube_context: Option<&str>,
    now: DateTime<Utc>,
) -> String {
    let mut lines = Vec::new();
    if !operators.logins.is_empty() {
        lines.push("OPERATORS".to_owned());
        lines.extend(operator_table(operators, now));
        if operators.active.is_none() {
            lines.push(String::new());
        }
    }
    if operators.active.is_none() {
        lines.push(no_login_in_use(kube_context));
    }
    lines.push(String::new());
    lines.push(cloud_line(cloud, now));
    lines.join("\n")
}

/// What mirrord connects with while no operator login is in use.
pub(super) fn no_login_in_use(kube_context: Option<&str>) -> String {
    match kube_context {
        Some(context) => {
            format!("No operator login in use. mirrord uses your kubeconfig context {context}.")
        }
        None => "No operator login in use, and no kubeconfig was found.".to_owned(),
    }
}

/// One header row and one row per login, columns padded to their widest value and separated by
/// two spaces, the active login marked with `*`.
fn operator_table(operators: &OperatorLogins, now: DateTime<Utc>) -> Vec<String> {
    let header = ["ACTIVE", "URL", "ACCOUNT", "EXPIRES"].map(str::to_owned);
    let rows = operators.logins.iter().map(|login| {
        let active = if operators.active.as_deref() == Some(&login.url) {
            "*"
        } else {
            ""
        };
        [
            active.to_owned(),
            login.host().to_owned(),
            login.account.clone(),
            expires_in(login.expires_at, now),
        ]
    });
    let rows = std::iter::once(header).chain(rows).collect::<Vec<_>>();
    let widths = rows.iter().fold([0; 4], |widths, row| {
        let mut widths = widths;
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
        widths
    });
    rows.iter()
        .map(|row| {
            let cells = row
                .iter()
                .zip(widths)
                .map(|(cell, width)| format!("{cell:<width$}"))
                .collect::<Vec<_>>();
            cells.join("  ").trim_end().to_owned()
        })
        .collect()
}

fn cloud_line(cloud: Option<&CloudLogin>, now: DateTime<Utc>) -> String {
    match cloud {
        Some(login) if login.expires_at > now => format!(
            "mirrord Cloud: {}, expires {}",
            login.email,
            expires_in(login.expires_at, now)
        ),
        Some(login) => format!("mirrord Cloud: {}, expired", login.email),
        None => "mirrord Cloud: not logged in".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;
    use mirrord_operator::sso::{OperatorLogin, OperatorLoginToken};

    use super::*;

    fn login(url: &str, expires_at: DateTime<Utc>) -> OperatorLogin {
        OperatorLogin {
            url: url.to_owned(),
            token: OperatorLoginToken::from("t".to_owned()),
            username: "dev@corp.example.com".to_owned(),
            account: "dev@corp.example.com".to_owned(),
            expires_at,
        }
    }

    /// The ticket's example output, word for word.
    #[test]
    fn renders_logins_as_a_table_with_the_cloud_login_below() {
        let now = Utc::now();
        let operators = OperatorLogins {
            logins: vec![
                login(
                    "https://mirrord.corp.example.com",
                    now + TimeDelta::hours(6),
                ),
                login(
                    "https://staging.mirrord.corp.example.com",
                    now - TimeDelta::hours(1),
                ),
            ],
            active: Some("https://mirrord.corp.example.com".to_owned()),
        };
        let cloud = CloudLogin {
            email: "dev@corp.example.com".to_owned(),
            expires_at: now + TimeDelta::hours(12),
        };

        assert_eq!(
            render(&operators, Some(&cloud), Some("staging-eu"), now),
            "OPERATORS\n\
             ACTIVE  URL                               ACCOUNT               EXPIRES\n\
             *       mirrord.corp.example.com          dev@corp.example.com  in 6 hours\n\
             \x20       staging.mirrord.corp.example.com  dev@corp.example.com  expired\n\
             \n\
             mirrord Cloud: dev@corp.example.com, expires in 12 hours"
        );
    }

    #[test]
    fn says_what_mirrord_uses_without_an_operator_login() {
        let now = Utc::now();
        let cloud = CloudLogin {
            email: "dev@corp.example.com".to_owned(),
            expires_at: now + TimeDelta::hours(12),
        };

        assert_eq!(
            render(
                &OperatorLogins::default(),
                Some(&cloud),
                Some("staging-eu"),
                now
            ),
            "No operator login in use. mirrord uses your kubeconfig context staging-eu.\n\
             \n\
             mirrord Cloud: dev@corp.example.com, expires in 12 hours"
        );
        assert_eq!(
            render(&OperatorLogins::default(), None, None, now),
            "No operator login in use, and no kubeconfig was found.\n\
             \n\
             mirrord Cloud: not logged in"
        );
    }

    /// Logins that are saved but none in use: the table still shows them, with the fallback line.
    #[test]
    fn lists_saved_logins_when_none_is_in_use() {
        let now = Utc::now();
        let operators = OperatorLogins {
            logins: vec![login(
                "https://mirrord.corp.example.com",
                now - TimeDelta::hours(1),
            )],
            active: None,
        };
        let cloud = CloudLogin {
            email: "dev@corp.example.com".to_owned(),
            expires_at: now - TimeDelta::hours(1),
        };

        assert_eq!(
            render(&operators, Some(&cloud), Some("staging-eu"), now),
            "OPERATORS\n\
             ACTIVE  URL                       ACCOUNT               EXPIRES\n\
             \x20       mirrord.corp.example.com  dev@corp.example.com  expired\n\
             \n\
             No operator login in use. mirrord uses your kubeconfig context staging-eu.\n\
             \n\
             mirrord Cloud: dev@corp.example.com, expired"
        );
    }
}
