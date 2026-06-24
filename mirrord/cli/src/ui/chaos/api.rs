use axum::{
    Extension, Json,
    extract::{Path, Request, State},
    middleware::Next,
    response::Response,
};
use mirrord_intproxy::session_monitor::chaos::{
    SessionId,
    rules::{ChaosRule, ChaosRuleRequest},
};
use mirrord_session_monitor_client::SessionClient;
use serde::Deserialize;
use tracing::Level;
use uuid::Uuid;

use super::ChaosResult;
use crate::ui::{AppState, chaos::error::ChaosApiError};

/// The route for the internal proxy session monitor server chaos rules.
const BASE_INTPROXY_CHAOS_ROUTE: &str = "/chaos/rules";

/// axum [`Path`] helper for the middleware.
///
/// Without this, the [`get_session_client_middleware`] has to be manually inspected (via `HashMap`)
/// for a `session_id` parameter in the url `Path`.
#[derive(Deserialize)]
pub(super) struct SessionPath {
    /// Required [`SessionId`] field to appear in [`Path`].
    session_id: SessionId,
}

/// axum [`Path`] helper for route handlers that only care about `rule_id`.
///
/// Some routes functions receive `.../{session_id}/{rule_id}`, but they're only interested in the
/// `rule_id`.
#[derive(Deserialize)]
pub(super) struct RulePath {
    /// Required `rule_id` field to appear in [`Path`].
    rule_id: Uuid,
}

/// Checks if the `session_id` is available in our [`AppState`] (which means that there's some
/// mirrord session running with this id), and then extracts the [`SessionClient`] that can be
/// used to make requests to the session monitor server of this particular session. Puts
/// this `SessionClient` as a request [`Extension`] and passes it along to the chaos route
/// handlers.
#[tracing::instrument(level = Level::DEBUG, skip(state, request, next))]
pub(super) async fn get_session_client_middleware(
    State(state): State<AppState>,
    Path(SessionPath { session_id }): Path<SessionPath>,
    mut request: Request,
    next: Next,
) -> ChaosResult<Response> {
    let sessions = state.sessions.read().await;

    match sessions.get(&session_id.to_string()) {
        Some(session) => {
            request.extensions_mut().insert(session.client.clone());
            Ok(next.run(request).await)
        }
        None => Err(ChaosApiError::SessionNotFound(session_id.to_string())),
    }
}

/// - `POST /chaos/rules/{session_id}?token={mirrord-ui-token}`: forwards the [`ChaosRule`] creation
///   to the intproxy session monitor, and returns the `ChaosRule` that was created;
#[tracing::instrument(level = Level::DEBUG, ret, err)]
pub(super) async fn post_create_rule(
    Extension(client): Extension<SessionClient>,
    Json(new_rule): Json<ChaosRuleRequest>,
) -> ChaosResult<Json<ChaosRule>> {
    let created_rule = client
        .post(format!("{BASE_INTPROXY_CHAOS_ROUTE}/"))
        .json(&new_rule)
        .send()
        .await?
        .json()
        .await?;

    Ok(Json(created_rule))
}

/// - `DELETE /chaos/rules/{session_id}?token={mirrord-ui-token}`: forwards the deletion of every
///   [`ChaosRule`] for this `session_id` to the session monitor.
#[tracing::instrument(level = Level::DEBUG, ret, err)]
pub(super) async fn delete_clear_session_rules(
    Extension(client): Extension<SessionClient>,
) -> ChaosResult<()> {
    client
        .delete(format!("{BASE_INTPROXY_CHAOS_ROUTE}/"))
        .send()
        .await?;

    Ok(())
}

/// - `GET /chaos/rules/{session_id}?token={mirrord-ui-token}`: gets every [`ChaosRule`] of this
///   `session_id` from the session monitor.
#[tracing::instrument(level = Level::DEBUG, ret, err)]
pub(super) async fn get_list_active_rules_for_session(
    Extension(client): Extension<SessionClient>,
) -> ChaosResult<Json<Vec<ChaosRule>>> {
    let response = client
        .get(format!("{BASE_INTPROXY_CHAOS_ROUTE}/"))
        .send()
        .await?
        .json()
        .await?;

    Ok(Json(response))
}

/// - `PUT /chaos/rules/{session_id}/{rule_id}?token={mirrord-ui-token}`: forwards the [`ChaosRule`]
///   update to the session monitor for this `session_id` and `rule_id`.
#[tracing::instrument(level = Level::DEBUG, ret, err)]
pub(super) async fn put_update_rule(
    Path(RulePath { rule_id }): Path<RulePath>,
    Extension(client): Extension<SessionClient>,
    Json(updated_rule): Json<ChaosRuleRequest>,
) -> ChaosResult<Json<ChaosRule>> {
    let old_rule = client
        .put(format!("{BASE_INTPROXY_CHAOS_ROUTE}/{rule_id}"))
        .json(&updated_rule)
        .send()
        .await?
        .json()
        .await?;

    Ok(Json(old_rule))
}

/// - `DELETE /chaos/rules/{session_id}/{rule_id}?token={mirrord-ui-token}`: forwards the deletion
///   of a [`ChaosRule`] with `id == rule_id` to the session monitor. [`ChaosRule`] with `id ==
///   rule_id`.
#[tracing::instrument(level = Level::DEBUG, ret, err)]
pub(super) async fn delete_rule(
    Path(RulePath { rule_id }): Path<RulePath>,
    Extension(client): Extension<SessionClient>,
) -> ChaosResult<Json<ChaosRule>> {
    let deleted_rule = client
        .delete(format!("{BASE_INTPROXY_CHAOS_ROUTE}/{rule_id}"))
        .send()
        .await?
        .json()
        .await?;

    Ok(Json(deleted_rule))
}

/// - `GET /chaos/rules/{session_id}/{rule_id}?token={mirrord-ui-token}`: gets the [`ChaosRule`]
///   with `id == rule_id` from the session monitor.
#[tracing::instrument(level = Level::DEBUG, ret, err)]
pub(super) async fn get_rule(
    Path(RulePath { rule_id }): Path<RulePath>,
    Extension(client): Extension<SessionClient>,
) -> ChaosResult<Json<ChaosRule>> {
    let found_rule = client
        .get(format!("{BASE_INTPROXY_CHAOS_ROUTE}/{rule_id}"))
        .send()
        .await?
        .json()
        .await?;

    Ok(Json(found_rule))
}
