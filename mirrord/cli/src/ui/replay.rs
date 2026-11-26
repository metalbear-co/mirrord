//! Defines the [`replay_router`], which forwards the UI's calls to the routes of a session that
//! list and replay its captured requests.

use axum::{
    Extension, Json, Router,
    extract::Path,
    http::StatusCode,
    middleware,
    routing::{get, post},
};
use mirrord_session_monitor_client::SessionClient;
use mirrord_session_monitor_protocol::{
    CapturedRequest, CapturedRequestSummary, REPLAY_REQUESTS_ROUTE, ReplayRun, SessionInfo,
};
use serde::Deserialize;
use tracing::Level;

use crate::{
    error::CliError,
    replay::{KeptMessage, ReplayedMessage, list_queue_messages, replay_queue_message},
    ui::{
        chaos::{api::get_session_client_middleware, error::ChaosApiError},
        server::AppState,
    },
};

/// Routes for `/api/replay/{...}`, authenticated like every other API route.
///
/// The session monitor of each mirrord session keeps the HTTP requests that the session received
/// for the local application. These routes forward to the session monitor of `session_id`:
///
/// - `GET /{session_id}`: lists the requests;
/// - `GET /{session_id}/{request_id}`: gets one request with its headers, body and replays;
/// - `POST /{session_id}/{request_id}/replay`: sends the request to the local application again,
///   unchanged, and returns the response;
/// - `GET /{session_id}/messages`: lists the queue messages that the operator kept for the
///   session;
/// - `POST /{session_id}/messages/{message_id}/replay`: publishes one of them to the session's
///   output queue again.
pub(super) fn replay_router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/{session_id}", get(list_requests))
        .route("/{session_id}/messages", get(list_messages))
        .route(
            "/{session_id}/messages/{message_id}/replay",
            post(replay_message),
        )
        .route("/{session_id}/{request_id}", get(get_request))
        .route("/{session_id}/{request_id}/replay", post(replay_request))
        .route_layer(middleware::from_fn_with_state(
            state,
            get_session_client_middleware,
        ))
}

/// The part of the path that names a captured request.
#[derive(Deserialize)]
struct RequestPath {
    request_id: u64,
}

#[tracing::instrument(level = Level::DEBUG, err)]
async fn list_requests(
    Extension(client): Extension<SessionClient>,
) -> Result<Json<Vec<CapturedRequestSummary>>, ChaosApiError> {
    let requests = client
        .get(REPLAY_REQUESTS_ROUTE)
        .send()
        .await?
        .json()
        .await?;

    Ok(Json(requests))
}

#[tracing::instrument(level = Level::DEBUG, err)]
async fn get_request(
    Extension(client): Extension<SessionClient>,
    Path(RequestPath { request_id }): Path<RequestPath>,
) -> Result<Json<CapturedRequest>, ChaosApiError> {
    let request = client
        .get(format!("{REPLAY_REQUESTS_ROUTE}/{request_id}"))
        .send()
        .await?
        .json()
        .await?;

    Ok(Json(request))
}

#[tracing::instrument(level = Level::DEBUG, err)]
async fn replay_request(
    Extension(client): Extension<SessionClient>,
    Path(RequestPath { request_id }): Path<RequestPath>,
) -> Result<Json<ReplayRun>, ChaosApiError> {
    let run = client
        .post(format!("{REPLAY_REQUESTS_ROUTE}/{request_id}/replay"))
        .send()
        .await?
        .json()
        .await?;

    Ok(Json(run))
}

/// The part of the path that names a queue message kept by the operator.
#[derive(Deserialize)]
struct MessagePath {
    message_id: u64,
}

/// The session's own description, which names its operator session key and kube context.
async fn session_info(client: &SessionClient) -> Result<SessionInfo, ChaosApiError> {
    Ok(client.get("/info").send().await?.json().await?)
}

/// A failed queue replay call, as the text that the UI shows.
fn to_upstream_error(error: CliError) -> ChaosApiError {
    let body = match error {
        CliError::Replay(reason) => reason,
        CliError::FeatureDisabledInOperatorError {
            feature,
            enable_hint,
        } => {
            format!("{feature} is turned off in the operator's Helm values; enable it{enable_hint}")
        }
        error => error.to_string(),
    };

    ChaosApiError::Upstream {
        status: StatusCode::BAD_GATEWAY,
        body,
    }
}

#[tracing::instrument(level = Level::DEBUG, err)]
async fn list_messages(
    Extension(client): Extension<SessionClient>,
) -> Result<Json<Vec<KeptMessage>>, ChaosApiError> {
    let info = session_info(&client).await?;
    let messages = list_queue_messages(&info)
        .await
        .map_err(to_upstream_error)?;

    Ok(Json(messages))
}

#[tracing::instrument(level = Level::DEBUG, err)]
async fn replay_message(
    Extension(client): Extension<SessionClient>,
    Path(MessagePath { message_id }): Path<MessagePath>,
) -> Result<Json<ReplayedMessage>, ChaosApiError> {
    let info = session_info(&client).await?;
    let replayed = replay_queue_message(&info, message_id)
        .await
        .map_err(to_upstream_error)?;

    Ok(Json(replayed))
}
