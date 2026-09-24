//! Resolution of the workload replica id the agent registers with sessions-manager.
//!
//! When the customer does not configure an id, the agent detects the cloud platform it runs on and
//! asks that platform's instance metadata for a replica-unique identifier.

mod ecs;

use std::time::Duration;

use http::Uri;
use http_body_util::{BodyExt, Empty};
use hyper::body::Bytes;
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use mirrord_agent_env::envs;
use serde::de::DeserializeOwned;
use tokio::time::timeout;

/// Bounds the whole metadata lookup, so that a missing or unreachable endpoint cannot stall agent
/// startup.
const METADATA_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, thiserror::Error)]
enum ReplicaIdError {
    #[error("metadata endpoint `{0}` is not a valid HTTP URI")]
    InvalidUri(String),
    #[error("metadata request failed: {0}")]
    Request(#[from] hyper_util::client::legacy::Error),
    #[error("metadata response body could not be read: {0}")]
    Body(#[from] hyper::Error),
    #[error("metadata endpoint responded with status {0}")]
    Status(hyper::StatusCode),
    #[error("metadata response is not the expected JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("metadata request timed out")]
    Timeout,
}

/// A hosting platform whose instance metadata can identify the replica the agent runs beside.
///
/// To support another platform, add a variant, detect it in [`Platform::detect`] and resolve its
/// id in [`Platform::replica_id`]. The id must be unique among the concurrently running replicas
/// of one service (e.g. the ECS task ARN).
#[derive(Debug)]
enum Platform {
    /// AWS ECS, on EC2 or Fargate. One replica is one ECS task.
    Ecs(ecs::Endpoint),
}

impl Platform {
    /// Recognizes the platform from the environment the platform injects into its workloads.
    fn detect() -> Option<Self> {
        ecs::Endpoint::from_env().map(Self::Ecs)
    }

    /// Queries the platform's metadata endpoint for the replica id.
    async fn replica_id(&self) -> Result<String, ReplicaIdError> {
        match self {
            Self::Ecs(endpoint) => endpoint.task_arn().await,
        }
    }
}

/// Resolves the id of the replica this agent runs beside.
///
/// In order of precedence:
/// 1. [`envs::REMOTE_SERVICE_REPLICA`], the customer-controlled id.
/// 2. The id reported by the detected hosting platform's metadata (see [`Platform`]).
/// 3. `HOSTNAME`.
///
/// Returns `None` when none of these is available.
pub(crate) async fn resolve_replica_id() -> Option<String> {
    let configured = envs::REMOTE_SERVICE_REPLICA
        .try_from_env()
        .expect("String environment variables are infallible");
    if configured.is_some() {
        return configured;
    }

    if let Some(platform) = Platform::detect() {
        let resolved = timeout(METADATA_TIMEOUT, platform.replica_id())
            .await
            .unwrap_or(Err(ReplicaIdError::Timeout));
        match resolved {
            Ok(replica_id) => return Some(replica_id),
            Err(error) => {
                tracing::warn!(?platform, %error, "Failed to resolve the replica id from platform metadata");
            }
        }
    }

    std::env::var("HOSTNAME").ok()
}

/// Sends a `GET` with the given headers to a plain-HTTP metadata endpoint and parses the JSON
/// response.
async fn get_json<T: DeserializeOwned>(
    uri: &str,
    headers: &[(&str, &str)],
) -> Result<T, ReplicaIdError> {
    let uri: Uri = uri
        .parse()
        .ok()
        .filter(|uri: &Uri| uri.scheme_str() == Some("http"))
        .ok_or_else(|| ReplicaIdError::InvalidUri(uri.to_owned()))?;

    let mut request = hyper::Request::get(uri);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let request = request
        .body(Empty::<Bytes>::new())
        .expect("URI is valid and headers are static");

    let response = Client::builder(TokioExecutor::new())
        .build_http()
        .request(request)
        .await?;
    if !response.status().is_success() {
        return Err(ReplicaIdError::Status(response.status()));
    }

    let body = response.into_body().collect().await?.to_bytes();
    Ok(serde_json::from_slice(&body)?)
}
