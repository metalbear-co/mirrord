/*
POST /chaos/rules/{session_id}: create rule, return rule object with assigned ID
GET /chaos/rules/{session_id}: list active rules for session
GET /chaos/rules/{session_id}/{rule_id}: get specific rule
PUT /chaos/rules/{session_id}/{rule_id}: update rule
DELETE /chaos/rules/{session_id}/{rule_id}: delete rule
DELETE /chaos/rules/{session_id}: clear all rules for session*/

use axum::{
    Json, Router,
    extract::{FromRequest, Path, State},
    routing::{post, put},
};
use uuid::Uuid;

use crate::session_monitor::{
    api::AppState,
    chaos::{api::error::ChaosApiError, rules::ChaosRuleRequest, *},
};

pub(crate) mod error;

type ChaosResult<T> = Result<T, ChaosApiError>;

impl FromRequest<AppState> for ChaosRule {
    type Rejection = ChaosApiError;

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
    Path(_): Path<SessionId>,
    State(state): State<AppState>,
    Json(new_rule): Json<ChaosRuleRequest>,
) -> ChaosResult<Json<ChaosRule>> {
    let new_rule = ChaosRule::try_from(new_rule)?;
    let rule_id = new_rule.id;

    let created_rule = state
        .chaos_tx
        .create_rule(new_rule)
        .ok_or(ChaosApiError::RuleAlreadyPresent(rule_id))?;

    Ok(Json(created_rule))
}

async fn get_list_active_rules_for_session(
    Path(_): Path<SessionId>,
    State(state): State<AppState>,
) -> ChaosResult<Json<ChaosRuleList>> {
    Ok(Json(state.chaos_tx.list_active_rules_for_session()))
}

async fn delete_clear_session_rules(
    Path(_): Path<SessionId>,
    State(state): State<AppState>,
) -> ChaosResult<()> {
    Ok(state.chaos_tx.clear_session_rules())
}

async fn put_update_rule(
    Path((_, rule_id)): Path<(SessionId, Uuid)>,
    State(state): State<AppState>,
    Json(new_rule): Json<ChaosRuleRequest>,
) -> ChaosResult<Json<ChaosRule>> {
    let replaced_rule = state
        .chaos_tx
        .update_rule(ChaosRule::try_from((rule_id, new_rule))?)
        .ok_or(ChaosApiError::RuleNotFound(rule_id))?;

    Ok(Json(replaced_rule))
}

async fn delete_rule(
    Path((_, rule_id)): Path<(SessionId, Uuid)>,
    State(state): State<AppState>,
) -> ChaosResult<Json<ChaosRule>> {
    let stored_rule = state
        .chaos_tx
        .delete_rule(rule_id)
        .ok_or(ChaosApiError::RuleNotFound(rule_id))?;

    Ok(Json(stored_rule))
}

async fn get_rule(
    Path((_, rule_id)): Path<(SessionId, Uuid)>,
    State(state): State<AppState>,
) -> ChaosResult<Json<ChaosRule>> {
    let stored_rule = state
        .chaos_tx
        .get_rule(rule_id)
        .ok_or(ChaosApiError::RuleNotFound(rule_id))?;

    Ok(Json(stored_rule))
}
