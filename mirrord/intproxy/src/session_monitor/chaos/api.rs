/*
POST /chaos/rules/{session_id}: create rule, return rule object with assigned ID
GET /chaos/rules/{session_id}: list active rules for session
GET /chaos/rules/{session_id}/{rule_id}: get specific rule
PUT /chaos/rules/{session_id}/{rule_id}: update rule
DELETE /chaos/rules/{session_id}/{rule_id}: delete rule
DELETE /chaos/rules/{session_id}: clear all rules for session*/

use anyhow::Context;
use axum::{
    Json, Router,
    extract::{FromRequest, Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{post, put},
};
use uuid::Uuid;

use crate::session_monitor::{
    api::AppState,
    chaos::{ChaosRule, ChaosRuleList, rules::ChaosRuleRequest},
};

type ChaosResult<T> = Result<T, ApiError>;

#[derive(Debug)]
pub(crate) struct ApiError(anyhow::Error);

impl FromRequest<AppState> for ChaosRule {
    type Rejection = ApiError;

    /// Create a new [`Self`](ChaosRule) from a [`ChaosRuleRequest`], verifying that the rule is
    /// valid. The selector must be inferred from the fields in `@value.selector`, and then checked
    /// for compatibility with the requested effect type in `@value.effect`.
    ///
    /// Will fail for unimplemented effects and selectors.
    async fn from_request(
        req: axum::extract::Request,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let Json(request) = Json::<ChaosRuleRequest>::from_request(req, state).await?;

        Ok(ChaosRule::try_from(request)?)
    }
}

impl<E> From<E> for ApiError
where
    E: Into<anyhow::Error>,
{
    fn from(err: E) -> Self {
        Self(err.into())
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Something went wrong: {}", self.0),
        )
            .into_response()
    }
}

// TODO(alex): ... we have a shared chaos state in the `AppState` of this thing.
//
// The `Receiver` side of this shared state has been passed to the `OutgoingProxy` (and whatever
// other background task we want), and shall live as the shared state of that task.
//
// > But alex, why a watcher channel? Why not share state with `Arc`?
//
// It's easier, the watcher keeps the most up-to-date info and is shareable, so we don't have to
// keep locking something whenever we want to see if a rule applies, we just need to check the
// channel.
pub(crate) fn chaos_router() -> Router<AppState> {
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
}

// TODO: creating a rule has to return the new rule after creation
async fn post_create_rule(
    Path(session_id): Path<Uuid>,
    State(state): State<AppState>,
    Json(new_rule): Json<ChaosRuleRequest>,
) -> ChaosResult<()> {
    state.chaos_tx.create_rule(ChaosRule::try_from(new_rule)?);

    Ok(())
}

async fn get_list_active_rules_for_session(
    Path(session_id): Path<Uuid>,
    State(state): State<AppState>,
) -> ChaosResult<Json<ChaosRuleList>> {
    Ok(Json(state.chaos_tx.list_active_rules_for_session()))
}

async fn delete_clear_session_rules(
    Path(session_id): Path<Uuid>,
    State(state): State<AppState>,
) -> ChaosResult<()> {
    Ok(state.chaos_tx.clear_session_rules())
}

// TODO: updating a rule has to return the old, deleted rule
async fn put_update_rule(
    Path((session_id, rule_id)): Path<(Uuid, Uuid)>,
    State(state): State<AppState>,
    Json(new_rule): Json<ChaosRuleRequest>,
) -> ChaosResult<()> {
    Ok(state.chaos_tx.update_rule(ChaosRule::try_from(new_rule)?))
}

// TODO: deleting a rule has to return the rule (like `.pop()`)
async fn delete_rule(
    Path((session_id, rule_id)): Path<(Uuid, Uuid)>,
    State(state): State<AppState>,
) -> ChaosResult<()> {
    Ok(state.chaos_tx.delete_rule(rule_id))
}

async fn get_rule(
    Path((session_id, rule_id)): Path<(Uuid, Uuid)>,
    State(state): State<AppState>,
) -> ChaosResult<Json<ChaosRule>> {
    Ok(Json(state.chaos_tx.get_rule(rule_id).context("not found")?))
}
