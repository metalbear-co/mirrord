//! Defines the [`chaos_router`] [`Router`] for handling the [`ChaosRule`] CRUD.
use std::collections::HashMap;

use axum::{
    Extension, Json, Router,
    extract::{Path, Request, State},
    middleware::{self, Next},
    response::Response,
    routing::{post, put},
};
use mirrord_intproxy::session_monitor::chaos::{
    SessionId,
    rules::{ChaosRule, ChaosRuleRequest},
};
use mirrord_session_monitor_client::SessionClient;
use tracing::Level;
use uuid::Uuid;

use crate::ui::{AppState, chaos::error::ChaosApiError};

mod error;

/// The route for the internal proxy session monitor server chaos rules.
const BASE_INTPROXY_CHAOS_ROUTE: &str = "/chaos/rules";

/// Alias for the return type of the chaos route handlers.
type ChaosResult<T> = Result<T, ChaosApiError>;

/// Routes for `/chaos/rules/{...}?token={mirrord-ui-token}`.
///
/// These routes are sort of a proxy between user requests and the intproxy session monitor server
/// that handles the [`ChaosRule`]s. Each mirrord session starts that server, and the user can send
/// requests to this router, with the desired `session_id` to manage the `ChaosRules`.
///
/// We make use of the [`get_session_client_middleware`] to simplify the route handlers, see the
/// middleware for more details.
///
/// - `POST /{session_id}`: creates a new rule **unconditionally** for the session;
/// - `DELETE /{session_id}`: deletes every rule of the session;
/// - `GET /{session_id}`: gets the list of rules for the session;
/// - `PUT /{session_id}/{rule_id}`: updates the rule for the session;
/// - `DELETE /{session_id}/{rule_id}`: deletes the rule of the session;
/// - `GET /{session_id}/{rule_id}`: gets the rule of the session;
pub(super) fn chaos_router(state: AppState) -> Router<AppState> {
    Router::new()
        .route(
            "/{session_id}",
            post(post_create_rule)
                .delete(delete_clear_session_rules)
                .get(get_list_active_rules_for_session),
        )
        .route(
            "/{session_id}/{rule_id}",
            put(put_update_rule).delete(delete_rule).get(get_rule),
        )
        .route_layer(middleware::from_fn_with_state(
            state,
            get_session_client_middleware,
        ))
}

/// Checks if the `session_id` is available in our [`AppState`] (which means that there's some
/// mirrord session running with this id), and then extracts the [`SessionClient`] that can be used
/// to make requests to the session monitor server of this particular session. Puts this
/// `SessionClient` as a request [`Extension`] and passes it along to the chaos route handlers.
#[tracing::instrument(level = Level::INFO, skip(state, request, next))]
async fn get_session_client_middleware(
    State(state): State<AppState>,
    Path(params): Path<HashMap<String, String>>,
    mut request: Request,
    next: Next,
) -> ChaosResult<Response> {
    let Some(session_id) = params.get("session_id") else {
        return Err(ChaosApiError::SessionNotFound("no session id".to_owned()));
    };

    let sessions = state.sessions.read().await;

    match sessions.get(&session_id.to_string()) {
        Some(session) => {
            request.extensions_mut().insert(session.client.clone());
            Ok(next.run(request).await)
        }
        None => Err(ChaosApiError::SessionNotFound(session_id.clone())),
    }
}

/// - `POST /chaos/rules/{session_id}?token={mirrord-ui-token}`: creates a new [`ChaosRule`] based
///   on the [`ChaosRuleRequest`] that we received. When a `ChaosRule` is built from a
///   `ChaosRuleRequest` it auto-generates a [`ChaosRule::id`], which makes every request here
///   create a unique `ChaosRule` (even if all the other fields are the same as another `ChaosRule`,
///   we only compare ids);
#[tracing::instrument(level = Level::INFO, ret, err)]
async fn post_create_rule(
    Path(session_id): Path<SessionId>,
    Extension(client): Extension<SessionClient>,
    Json(new_rule): Json<ChaosRuleRequest>,
) -> ChaosResult<Json<ChaosRule>> {
    let created_rule = client
        .post(format!("{BASE_INTPROXY_CHAOS_ROUTE}/{session_id}"))
        .json(&new_rule)
        .send()
        .await?
        .json()
        .await?;

    Ok(Json(created_rule))
}

/// - `DELETE /chaos/rules/{session_id}?token={mirrord-ui-token}`: deletes every rule for this
///   `session_id`.
#[tracing::instrument(level = Level::INFO, ret, err)]
async fn delete_clear_session_rules(
    Path(session_id): Path<SessionId>,
    Extension(client): Extension<SessionClient>,
) -> ChaosResult<()> {
    client
        .delete(format!("{BASE_INTPROXY_CHAOS_ROUTE}/{session_id}"))
        .send()
        .await?;

    Ok(())
}

/// - `GET /chaos/rules/{session_id}?token={mirrord-ui-token}`: returns the list of every
///   [`ChaosRule`] for this `session_id`.
#[tracing::instrument(level = Level::INFO, ret, err)]
async fn get_list_active_rules_for_session(
    Path(session_id): Path<SessionId>,
    Extension(client): Extension<SessionClient>,
) -> ChaosResult<Json<Vec<ChaosRule>>> {
    let response = client
        .get(format!("{BASE_INTPROXY_CHAOS_ROUTE}/{session_id}"))
        .send()
        .await?
        .json()
        .await?;

    Ok(Json(response))
}

/// - `PUT /chaos/rules/{session_id}/{rule_id}?token={mirrord-ui-token}`: updates the [`ChaosRule`]
///   that matches this `rule_id`. When we create a `ChaosRule` from a pair of (`rule_id`,
///   [`ChaosRuleRequest`]), we can replace the `ChaosRule` whose `id` matches the `rule_id`.
#[tracing::instrument(level = Level::INFO, ret, err)]
async fn put_update_rule(
    Path((session_id, rule_id)): Path<(SessionId, Uuid)>,
    Extension(client): Extension<SessionClient>,
    Json(updated_rule): Json<ChaosRuleRequest>,
) -> ChaosResult<Json<ChaosRule>> {
    let old_rule = client
        .put(format!(
            "{BASE_INTPROXY_CHAOS_ROUTE}/{session_id}/{rule_id}"
        ))
        .json(&updated_rule)
        .send()
        .await?
        .json()
        .await?;

    Ok(Json(old_rule))
}

/// - `DELETE /chaos/rules/{session_id}/{rule_id}?token={mirrord-ui-token}`: deletes the
///   [`ChaosRule`] with `id == rule_id`.
#[tracing::instrument(level = Level::INFO, ret, err)]
async fn delete_rule(
    Path((session_id, rule_id)): Path<(SessionId, Uuid)>,
    Extension(client): Extension<SessionClient>,
) -> ChaosResult<Json<ChaosRule>> {
    let deleted_rule = client
        .delete(format!(
            "{BASE_INTPROXY_CHAOS_ROUTE}/{session_id}/{rule_id}"
        ))
        .send()
        .await?
        .json()
        .await?;

    Ok(Json(deleted_rule))
}

/// - `GET /chaos/rules/{session_id}/{rule_id}?token={mirrord-ui-token}`: returns the [`ChaosRule`]
///   with `id == rule_id`.
#[tracing::instrument(level = Level::INFO, ret, err)]
async fn get_rule(
    Path((session_id, rule_id)): Path<(SessionId, Uuid)>,
    Extension(client): Extension<SessionClient>,
) -> ChaosResult<Json<ChaosRule>> {
    let found_rule = client
        .get(format!(
            "{BASE_INTPROXY_CHAOS_ROUTE}/{session_id}/{rule_id}"
        ))
        .send()
        .await?
        .json()
        .await?;

    Ok(Json(found_rule))
}
