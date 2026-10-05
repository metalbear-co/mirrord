//! Starting a provisional trial through the MetalBear app's agent signup endpoint.
//!
//! The signup creates an organization with a trial license and an operator API key. The user takes
//! ownership of the organization by opening the claim URL, the API key keeps working after that.

use chrono::{DateTime, Utc};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

use super::error::OperatorInstallError;

#[derive(Serialize)]
struct SignupRequest<'a> {
    /// Identifies what started the trial.
    agent: &'a str,
    /// Shown on the claim page, so the user can tell which cluster the trial belongs to.
    #[serde(skip_serializing_if = "Option::is_none")]
    cluster_hint: Option<&'a str>,
}

/// A started trial.
///
/// Deliberately not [`Debug`], to keep the API key out of logs.
#[derive(Deserialize)]
pub(super) struct Trial {
    pub(super) api_key: String,
    pub(super) trial_ends_at: DateTime<Utc>,
    pub(super) claim_url: String,
}

pub(super) async fn start_trial(
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
