//! Defines the [`replay_router`], which forwards the UI's calls to the routes of a session that
//! list and replay its captured requests.

use axum::{
    Extension, Router,
    extract::Path,
    http::header::CONTENT_TYPE,
    middleware,
    response::IntoResponse,
    routing::{get, post},
};
use bytes::Bytes;
use mirrord_session_monitor_client::SessionClient;
use mirrord_session_monitor_protocol::REPLAY_REQUESTS_ROUTE;
use serde::Deserialize;
use tracing::Level;

use crate::ui::{
    chaos::{api::get_session_client_middleware, error::ChaosApiError},
    server::AppState,
};

/// Routes for `/api/replay/{...}`, authenticated like every other API route.
///
/// The session monitor of each mirrord session keeps the HTTP requests that the session received
/// for the local application. These routes forward to the session monitor of `session_id`:
///
/// - `GET /{session_id}`: lists the requests;
/// - `GET /{session_id}/{request_id}`: gets one request with its headers, body and replays;
/// - `POST /{session_id}/{request_id}/replay`: sends the request to the local application again,
///   unchanged, and returns the response.
pub(super) fn replay_router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/{session_id}", get(list_requests))
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

/// Returns the JSON that the session answered, unchanged.
///
/// Parsing it would fail on a value that a newer session added, and would cost a copy of an answer
/// that holds bodies.
fn json(body: Bytes) -> impl IntoResponse {
    ([(CONTENT_TYPE, "application/json")], body)
}

#[tracing::instrument(level = Level::DEBUG, err)]
async fn list_requests(
    Extension(client): Extension<SessionClient>,
) -> Result<impl IntoResponse, ChaosApiError> {
    let requests = client
        .get(REPLAY_REQUESTS_ROUTE)
        .send()
        .await?
        .bytes()
        .await?;

    Ok(json(requests))
}

#[tracing::instrument(level = Level::DEBUG, err)]
async fn get_request(
    Extension(client): Extension<SessionClient>,
    Path(RequestPath { request_id }): Path<RequestPath>,
) -> Result<impl IntoResponse, ChaosApiError> {
    let request = client
        .get(format!("{REPLAY_REQUESTS_ROUTE}/{request_id}"))
        .send()
        .await?
        .bytes()
        .await?;

    Ok(json(request))
}

#[tracing::instrument(level = Level::DEBUG, err)]
async fn replay_request(
    Extension(client): Extension<SessionClient>,
    Path(RequestPath { request_id }): Path<RequestPath>,
) -> Result<impl IntoResponse, ChaosApiError> {
    let run = client
        .post(format!("{REPLAY_REQUESTS_ROUTE}/{request_id}/replay"))
        .send()
        .await?
        .bytes()
        .await?;

    Ok(json(run))
}
