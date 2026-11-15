//! `cloud/*`: sessions of the developer's organization, from the MetalBear Cloud sessions-manager.
//!
//! Serverless targets run without an operator, so there's no cluster resource that lists their
//! sessions. Instead, the sessions-manager the developer is logged in to with `mirrord login`
//! lists the live sessions of their organization. It's the same sessions-manager, and the same
//! login, that `mirrord exec` connects a serverless target through.
//!
//! The frontend polls this next to `operator/sessions`. While it reports anything but
//! [`CloudStatus::LoggedOut`], the developer uses MetalBear Cloud, and a missing operator or kube
//! context is expected rather than an error.

use std::{
    sync::Arc,
    time::{Duration, UNIX_EPOCH},
};

use chrono::{DateTime, Utc};
use mirrord_sessions_manager_client::{
    DirectTransport, SESSIONS_MANAGER_URL_ENV, SessionOwner, SessionSummary,
    SessionsManagerEndpoint,
};
use serde::Serialize;
use tracing::warn;

use crate::{
    data::{AuthStore, StoredLogin},
    login::issuer,
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
enum CloudStatus {
    Available,
    /// No unexpired `mirrord login` token, so the developer doesn't use MetalBear Cloud.
    LoggedOut,
    /// Logged in, but the sessions couldn't be listed.
    Unavailable,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CloudSessionsResponse {
    status: CloudStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    /// The developer `mirrord login` signed in, so the frontend can tell their sessions apart.
    #[serde(skip_serializing_if = "Option::is_none")]
    email: Option<String>,
    sessions: Vec<CloudSession>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CloudSession {
    id: String,
    environment: String,
    service: String,
    replica_id: String,
    user_session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    owner: Option<CloudSessionOwner>,
    /// RFC 3339, like the operator sessions' `createdAt`.
    created_at: String,
    connected: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CloudSessionOwner {
    user_id: String,
    email: String,
}

impl From<SessionOwner> for CloudSessionOwner {
    fn from(owner: SessionOwner) -> Self {
        Self {
            user_id: owner.user_id,
            email: owner.email,
        }
    }
}

impl From<SessionSummary> for CloudSession {
    fn from(session: SessionSummary) -> Self {
        let created_at: DateTime<Utc> =
            (UNIX_EPOCH + Duration::from_secs(session.started_at)).into();
        Self {
            id: session.id,
            environment: session.scope.environment,
            service: session.scope.service,
            replica_id: session.agent_replica_id.to_string(),
            user_session_id: session.user_session_id,
            owner: session.owner.map(CloudSessionOwner::from),
            created_at: created_at.to_rfc3339(),
            connected: session.connected,
        }
    }
}

impl CloudSessionsResponse {
    fn unavailable(email: Option<String>, reason: String) -> Self {
        warn!("{reason}");
        Self {
            status: CloudStatus::Unavailable,
            reason: Some(reason),
            email,
            sessions: Vec::new(),
        }
    }
}

pub(super) async fn cloud_sessions() -> axum::Json<CloudSessionsResponse> {
    let cloud_url = match StoredLogin::cloud_url() {
        Ok(url) => url,
        Err(error) => {
            return axum::Json(CloudSessionsResponse::unavailable(
                None,
                format!("MetalBear Cloud URL is invalid: {error}"),
            ));
        }
    };

    let login = match AuthStore::login_token(issuer(&cloud_url)).await {
        Ok(Some(login)) => login,
        Ok(None) => {
            return axum::Json(CloudSessionsResponse {
                status: CloudStatus::LoggedOut,
                reason: None,
                email: None,
                sessions: Vec::new(),
            });
        }
        Err(error) => {
            return axum::Json(CloudSessionsResponse::unavailable(
                None,
                format!("Could not read your mirrord login: {error}"),
            ));
        }
    };
    let email = Some(login.email);

    // `mirrord ui` has no mirrord config, so only the environment can point it at another
    // sessions-manager than MetalBear Cloud's.
    let explicit_url = std::env::var(SESSIONS_MANAGER_URL_ENV).ok();
    let sessions = async {
        let endpoint = SessionsManagerEndpoint::resolve(explicit_url.as_deref())?;
        DirectTransport::intproxy_from_env(endpoint, Some(Arc::new(StoredLogin)))?
            .list_sessions()
            .await
    }
    .await;

    let response = match sessions {
        Ok(list) => CloudSessionsResponse {
            status: CloudStatus::Available,
            reason: None,
            email,
            sessions: list.sessions.into_iter().map(CloudSession::from).collect(),
        },
        Err(error) => CloudSessionsResponse::unavailable(
            email,
            format!("Could not list MetalBear Cloud sessions: {error}"),
        ),
    };
    axum::Json(response)
}
