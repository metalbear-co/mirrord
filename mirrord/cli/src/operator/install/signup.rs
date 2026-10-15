//! `mirrord operator signup`: starts a provisional trial through the MetalBear app's agent signup
//! endpoint, for `mirrord operator install --api-key`.

use chrono::{DateTime, Utc};
use mirrord_analytics::AiAgent;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

use super::{Connection, USER_AGENT, cluster, error::OperatorInstallError};
use crate::config::OperatorSignupArgs;

#[derive(Serialize)]
struct SignupRequest<'a> {
    /// Identifies what started the trial.
    agent: &'a str,
    /// Shown on the claim page, so the user can tell which cluster the trial belongs to.
    #[serde(skip_serializing_if = "Option::is_none")]
    cluster_hint: Option<&'a str>,
}

/// A started trial, deliberately not [`Debug`] to keep the API key out of logs.
#[derive(Deserialize)]
struct Trial {
    api_key: String,
    trial_ends_at: DateTime<Utc>,
    claim_url: String,
}

pub(crate) async fn operator_signup(args: OperatorSignupArgs) -> Result<(), OperatorInstallError> {
    let OperatorSignupArgs {
        cluster_hint,
        no_hint,
        context,
        app_url,
    } = args;

    let cluster_hint = match (cluster_hint, no_hint) {
        (Some(cluster_hint), _) => Some(cluster_hint),
        (None, true) => None,
        (None, false) => match Connection::new(context).await {
            Ok(connection) => cluster::cluster_id(&connection.client).await,
            Err(error) => {
                tracing::debug!(?error, "no cluster to name in the signup");
                None
            }
        },
    };

    let http = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .map_err(OperatorInstallError::HttpClient)?;
    let agent = match AiAgent::detect() {
        Some(AiAgent::ClaudeCode) => "claude-code",
        Some(AiAgent::Cursor) => "cursor",
        Some(AiAgent::Codex) => "codex",
        Some(AiAgent::GeminiCli) => "gemini-cli",
        Some(AiAgent::Amp) => "amp",
        None => USER_AGENT,
    };
    let trial = start_trial(&http, &app_url, agent, cluster_hint.as_deref()).await?;

    println!(
        "Started a mirrord Enterprise trial under a new organization. It ends on {}.\n\nClaim the \
        organization: {}\n\nInstall the operator with the trial's API key, which the claim keeps \
        working:\n\n  mirrord operator install --api-key {}",
        trial.trial_ends_at.format("%B %-d, %Y"),
        trial.claim_url,
        trial.api_key,
    );

    Ok(())
}

async fn start_trial(
    http: &reqwest::Client,
    app_url: &str,
    agent: &str,
    cluster_hint: Option<&str>,
) -> Result<Trial, OperatorInstallError> {
    let response = http
        .post(format!(
            "{}/api/v1/agent/signup",
            app_url.trim_end_matches('/')
        ))
        .json(&SignupRequest {
            agent,
            cluster_hint,
        })
        .send()
        .await
        .map_err(OperatorInstallError::SignupRequest)?;

    match response.status() {
        status if status.is_success() => response
            .json()
            .await
            .map_err(OperatorInstallError::SignupRequest),
        StatusCode::TOO_MANY_REQUESTS => Err(OperatorInstallError::SignupRateLimited),
        StatusCode::SERVICE_UNAVAILABLE => Err(OperatorInstallError::SignupUnavailable),
        status => Err(OperatorInstallError::SignupFailed {
            status,
            body: response.text().await.unwrap_or_default(),
        }),
    }
}
