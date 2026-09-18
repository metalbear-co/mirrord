//! `mirrord trial start`: provisions a mirrord for Teams trial without a human in the loop.
//!
//! The open-source walls in [`crate::connection`] point AI coding agents here. An agent driving
//! the CLI can act on a command it sees in its own tool output, but can only follow a link by
//! leaving the loop it is in, so the trial has to exist as a command and not only as a
//! documented HTTP call.
//!
//! The organization this creates is provisional: it expires unless a human opens the returned
//! claim URL, which is why surfacing that URL matters more than any other part of the output.

use std::time::Duration;

use mirrord_analytics::AiAgent;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

use crate::{CliError, CliResult};

const SIGNUP_URL: &str = "https://app.metalbear.com/api/v1/agent/signup";

#[derive(Serialize)]
struct SignupRequest {
    agent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    developer_email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cluster_hint: Option<String>,
}

#[derive(Deserialize, Serialize)]
struct SignupResponse {
    organization_id: String,
    api_key: String,
    license_type: String,
    trial_ends_at: String,
    claim_code: String,
    claim_url: String,
    instructions_url: String,
}

/// Names the agent for attribution on the signup, falling back to the CLI itself when no agent
/// is detected. Matches the identifiers used by the `ai_agent` analytics property.
fn agent_name() -> String {
    match AiAgent::detect() {
        Some(AiAgent::ClaudeCode) => "claude-code",
        Some(AiAgent::Cursor) => "cursor",
        Some(AiAgent::Codex) => "codex",
        Some(AiAgent::GeminiCli) => "gemini-cli",
        Some(AiAgent::Amp) => "amp",
        None => "mirrord-cli",
    }
    .to_owned()
}

/// A stalled proxy or server must not leave the command hanging: an agent waiting on it has no
/// way to tell a slow signup from a dead one.
const SIGNUP_TIMEOUT: Duration = Duration::from_secs(30);

/// With `--json`, stdout carries only the JSON document, so a failure has to be serialized there
/// too. The human-readable diagnostic still goes to stderr through the normal error path.
pub async fn start(
    json: bool,
    developer_email: Option<String>,
    cluster_hint: Option<String>,
) -> CliResult<()> {
    match signup(json, developer_email, cluster_hint).await {
        Ok(()) => Ok(()),
        Err(error) => {
            if json {
                println!("{}", serde_json::json!({ "error": error.to_string() }));
            }

            Err(error)
        }
    }
}

async fn signup(
    json: bool,
    developer_email: Option<String>,
    cluster_hint: Option<String>,
) -> CliResult<()> {
    let request = SignupRequest {
        agent: agent_name(),
        developer_email,
        cluster_hint,
    };

    let client = reqwest::Client::builder()
        .timeout(SIGNUP_TIMEOUT)
        .build()
        .map_err(|error| CliError::TrialSignupFailed(error.to_string()))?;

    let response = client
        .post(SIGNUP_URL)
        .json(&request)
        .send()
        .await
        .map_err(|error| CliError::TrialSignupFailed(error.to_string()))?;

    match response.status() {
        StatusCode::TOO_MANY_REQUESTS => return Err(CliError::TrialSignupRateLimited),
        StatusCode::SERVICE_UNAVAILABLE => return Err(CliError::TrialSignupUnavailable),
        status if !status.is_success() => {
            return Err(CliError::TrialSignupFailed(format!("HTTP {status}")));
        }
        _ => {}
    }

    let signup: SignupResponse = response
        .json()
        .await
        .map_err(|error| CliError::TrialSignupFailed(error.to_string()))?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&signup)
                .map_err(|error| CliError::TrialSignupFailed(error.to_string()))?
        );
        return Ok(());
    }

    print_recipe(&signup);

    Ok(())
}

/// The trial is only usable once the operator is installed with the minted key, so the output
/// carries the whole recipe rather than the key alone.
fn print_recipe(signup: &SignupResponse) {
    println!(
        "Started a mirrord for Teams trial ({}).",
        signup.license_type
    );
    let trial_ends_on = signup
        .trial_ends_at
        .split_once('T')
        .map_or(signup.trial_ends_at.as_str(), |(date, _)| date);
    println!("It runs until {trial_ends_on}.");
    println!();
    println!("1. Install the operator into the cluster:");
    println!();
    println!("   helm repo add metalbear https://metalbear-co.github.io/charts");
    println!("   helm repo update metalbear");
    println!(
        "   helm install --set cloud.apiKey.key={} --set operator.previewEnv=true \\",
        signup.api_key
    );
    println!("     mirrord-operator metalbear/mirrord-operator");
    println!();
    println!("2. Give this claim link to the person you are working for:");
    println!();
    println!("   {}", signup.claim_url);
    println!();
    println!(
        "The organization is provisional until someone opens that link, and an unclaimed trial \
        expires with nobody able to renew it. Sign up once per cluster and keep handing over the \
        same link. Treat the API key as a secret: it belongs in the Helm install, not in a \
        repository or a commit."
    );
    println!();
    println!("More detail: {}", signup.instructions_url);
}
