//! `mirrord whoami`: which operator mirrord uses, the other operator logins, and the mirrord Cloud
//! login.

use chrono::{DateTime, Utc};
use mirrord_kube::api::kubernetes::resolve_kube_context;
use serde::Serialize;

use super::{LoginError, expiry::expires_in, issuer};
use crate::{
    config::{Format, WhoamiArgs},
    data::{AuthStore, OperatorLogins, StoredLogin},
};

/// Implementation of the `mirrord whoami` command.
pub(crate) async fn whoami_command(args: WhoamiArgs) -> Result<(), LoginError> {
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

    let now = Utc::now();
    let output = match args.output {
        None => render(&operators, cloud.as_ref(), kube_context.as_deref(), now),
        Some(Format::Json) => render_json(&operators, cloud.as_ref(), kube_context.as_deref(), now),
    };
    println!("{output}");
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

/// `mirrord whoami -o json`: the facts of the table, for the IDE extensions. Every field is always
/// present, `null` when there is nothing to show, so a reader never has to tell a missing field
/// from an older CLI.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WhoamiJson<'a> {
    /// Active login first, then the others from the most recent login to the oldest.
    operators: Vec<OperatorLoginJson<'a>>,
    /// URL of the operator login mirrord uses, kept when that login expired: it is the URL to log
    /// in to again. `null` when mirrord uses the kubeconfig.
    active_operator: Option<&'a str>,
    /// The kubeconfig context mirrord uses when no operator login is in use, `null` when no
    /// kubeconfig was found.
    kube_context: Option<&'a str>,
    cloud: Option<CloudLoginJson<'a>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OperatorLoginJson<'a> {
    /// What `mirrord login --url` and `mirrord logout --url` take.
    url: &'a str,
    /// How the table and the messages name the operator.
    host: &'a str,
    account: &'a str,
    expires_at: DateTime<Utc>,
    /// The table's EXPIRES column: `in 6 hours`, or `expired`.
    expires_in: String,
    active: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CloudLoginJson<'a> {
    email: &'a str,
    expires_at: DateTime<Utc>,
    expires_in: String,
}

fn render_json(
    operators: &OperatorLogins,
    cloud: Option<&CloudLogin>,
    kube_context: Option<&str>,
    now: DateTime<Utc>,
) -> String {
    let output = WhoamiJson {
        operators: operators
            .logins
            .iter()
            .map(|login| OperatorLoginJson {
                url: &login.url,
                host: login.host(),
                account: &login.account,
                expires_at: login.expires_at,
                expires_in: expires_in(login.expires_at, now),
                active: operators.active.as_deref() == Some(login.url.as_str()),
            })
            .collect(),
        // A selected URL with no saved login behind it is not used by any command, which fall back
        // to the kubeconfig, so it is not reported as in use either.
        active_operator: operators
            .active
            .as_deref()
            .filter(|url| operators.logins.iter().any(|login| login.url == *url)),
        kube_context,
        cloud: cloud.map(|login| CloudLoginJson {
            email: &login.email,
            expires_at: login.expires_at,
            expires_in: expires_in(login.expires_at, now),
        }),
    };
    serde_json::to_string(&output)
        .expect("strings, booleans and timestamps always serialize to JSON")
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
    use clap::Parser;
    use mirrord_operator::sso::{OperatorLogin, OperatorLoginToken};

    use super::*;
    use crate::config::{Cli, Commands};

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

    /// The IDE extensions run `mirrord whoami -o json`; without the flag the table is printed.
    #[test]
    fn dash_o_json_selects_the_json_output() {
        let json = Cli::try_parse_from(["mirrord", "whoami", "-o", "json"]).unwrap();
        assert!(matches!(
            json.commands,
            Commands::Whoami(WhoamiArgs {
                output: Some(Format::Json)
            })
        ));

        let table = Cli::try_parse_from(["mirrord", "whoami"]).unwrap();
        assert!(matches!(
            table.commands,
            Commands::Whoami(WhoamiArgs { output: None })
        ));
    }

    /// The JSON the IDE extensions read holds what the table shows, with the URL each login is
    /// logged in and out with, and the active URL kept for an expired login so the IDE can offer
    /// to log in to it again.
    #[test]
    fn renders_the_same_logins_as_json() {
        let now = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let operators = OperatorLogins {
            logins: vec![
                login(
                    "https://mirrord.corp.example.com",
                    now - TimeDelta::hours(1),
                ),
                login(
                    "https://staging.mirrord.corp.example.com",
                    now + TimeDelta::hours(6),
                ),
            ],
            active: Some("https://mirrord.corp.example.com".to_owned()),
        };
        let cloud = CloudLogin {
            email: "dev@corp.example.com".to_owned(),
            expires_at: now + TimeDelta::hours(12),
        };

        let json: serde_json::Value = serde_json::from_str(&render_json(
            &operators,
            Some(&cloud),
            Some("staging-eu"),
            now,
        ))
        .unwrap();

        assert_eq!(
            json,
            serde_json::json!({
                "operators": [
                    {
                        "url": "https://mirrord.corp.example.com",
                        "host": "mirrord.corp.example.com",
                        "account": "dev@corp.example.com",
                        "expiresAt": "2027-01-15T07:00:00Z",
                        "expiresIn": "expired",
                        "active": true,
                    },
                    {
                        "url": "https://staging.mirrord.corp.example.com",
                        "host": "staging.mirrord.corp.example.com",
                        "account": "dev@corp.example.com",
                        "expiresAt": "2027-01-15T14:00:00Z",
                        "expiresIn": "in 6 hours",
                        "active": false,
                    },
                ],
                "activeOperator": "https://mirrord.corp.example.com",
                "kubeContext": "staging-eu",
                "cloud": {
                    "email": "dev@corp.example.com",
                    "expiresAt": "2027-01-15T20:00:00Z",
                    "expiresIn": "in 12 hours",
                },
            })
        );
    }

    /// A store whose selected URL has no saved login behind it (left by an older CLI or a hand
    /// edit) uses the kubeconfig, so the JSON reports no operator in use and every login inactive.
    #[test]
    fn json_ignores_a_selected_url_without_a_login() {
        let now = Utc::now();
        let operators = OperatorLogins {
            logins: vec![login(
                "https://staging.mirrord.corp.example.com",
                now + TimeDelta::hours(6),
            )],
            active: Some("https://mirrord.corp.example.com".to_owned()),
        };

        let json: serde_json::Value =
            serde_json::from_str(&render_json(&operators, None, Some("staging-eu"), now)).unwrap();

        assert_eq!(json.get("activeOperator"), Some(&serde_json::Value::Null));
        assert_eq!(
            json.pointer("/operators/0/active"),
            Some(&serde_json::Value::Bool(false))
        );
    }

    /// No logins at all: every field is still there, so a reader tells "nothing saved" from an
    /// older CLI that prints no JSON.
    #[test]
    fn renders_json_without_logins() {
        let json: serde_json::Value = serde_json::from_str(&render_json(
            &OperatorLogins::default(),
            None,
            None,
            Utc::now(),
        ))
        .unwrap();

        assert_eq!(
            json,
            serde_json::json!({
                "operators": [],
                "activeOperator": null,
                "kubeContext": null,
                "cloud": null,
            })
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
