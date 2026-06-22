//! Defines the route handlers for the [`super::chaos_router`] for dealing with the
//! [`ChaosRule`] CRUD on the intproxy session monitor side.
use axum::{
    Json,
    extract::{FromRequest, Path, State},
};
use tracing::Level;
use uuid::Uuid;

use crate::session_monitor::{
    api::AppState,
    chaos::{
        api::error::ChaosApiError,
        rules::{ChaosRule, ChaosRuleRequest},
    },
};

pub(crate) mod error;

/// Alias for the return type of the session monitor chaos route handlers.
///
/// Same name as the ui `ChaosResult`, but the `ChaosApiError` is different!
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

/// - `POST /chaos/rules/`: creates a new [`ChaosRule`] based on the [`ChaosRuleRequest`] that we
///   received. When a `ChaosRule` is built from a `ChaosRuleRequest` it auto-generates a
///   [`ChaosRule::id`], which makes every request here create a unique `ChaosRule` (even if all the
///   other fields are the same as another `ChaosRule`, we only compare ids);
#[tracing::instrument(level = Level::INFO, skip(state), ret, err)]
pub(super) async fn post_create_rule(
    State(state): State<AppState>,
    Json(new_rule): Json<ChaosRuleRequest>,
) -> ChaosResult<Json<ChaosRule>> {
    let new_rule = ChaosRule::try_from(new_rule)?;
    let rule_id = new_rule.id;

    let created_rule = state
        .chaos_tx
        .create_rule(new_rule)
        .ok_or(ChaosApiError::RuleAlreadyPresent(rule_id))?;

    report_create_rule(&state, &created_rule);

    Ok(Json(created_rule))
}

/// - `DELETE /chaos/rules/`: deletes every rule.
#[tracing::instrument(level = Level::INFO, skip(state), ret, err)]
pub(super) async fn delete_clear_session_rules(State(state): State<AppState>) -> ChaosResult<()> {
    state.chaos_tx.clear_session_rules();

    report_clear_session_rules(&state);

    Ok(())
}

/// - `GET /chaos/rules/`: returns the list of every [`ChaosRule`].
#[tracing::instrument(level = Level::INFO, skip(state), ret, err)]
pub(super) async fn get_list_active_rules_for_session(
    State(state): State<AppState>,
) -> ChaosResult<Json<Vec<ChaosRule>>> {
    Ok(Json(state.chaos_tx.list_active_rules_for_session()))
}

/// - `PUT /chaos/rules/{rule_id}`: updates the [`ChaosRule`] that matches this `rule_id`. When we
///   create a `ChaosRule` from a pair of (`rule_id`, [`ChaosRuleRequest`]), we can replace the
///   `ChaosRule` whose `id` matches the `rule_id`.
#[tracing::instrument(level = Level::INFO, skip(state), ret, err)]
pub(super) async fn put_update_rule(
    Path(rule_id): Path<Uuid>,
    State(state): State<AppState>,
    Json(new_rule): Json<ChaosRuleRequest>,
) -> ChaosResult<Json<ChaosRule>> {
    let new_rule = ChaosRule::try_from((rule_id, new_rule))?;
    report_create_rule(&state, &new_rule);

    let replaced_rule = state
        .chaos_tx
        .update_rule(new_rule)
        .ok_or(ChaosApiError::RuleNotFound(rule_id))?;
    report_delete_rule(&state, &replaced_rule);

    Ok(Json(replaced_rule))
}

/// - `DELETE /chaos/rules/{rule_id}`: deletes the [`ChaosRule`] with `id == rule_id`.
#[tracing::instrument(level = Level::INFO, skip(state), ret, err)]
pub(super) async fn delete_rule(
    Path(rule_id): Path<Uuid>,
    State(state): State<AppState>,
) -> ChaosResult<Json<ChaosRule>> {
    let stored_rule = state
        .chaos_tx
        .delete_rule(rule_id)
        .ok_or(ChaosApiError::RuleNotFound(rule_id))?;

    report_delete_rule(&state, &stored_rule);

    Ok(Json(stored_rule))
}

/// - `GET /chaos/rules/{rule_id}`: returns the [`ChaosRule`] with `id == rule_id`.
#[tracing::instrument(level = Level::INFO, skip(state), ret, err)]
pub(super) async fn get_rule(
    Path(rule_id): Path<Uuid>,
    State(state): State<AppState>,
) -> ChaosResult<Json<ChaosRule>> {
    let stored_rule = state
        .chaos_tx
        .get_rule(rule_id)
        .ok_or(ChaosApiError::RuleNotFound(rule_id))?;

    Ok(Json(stored_rule))
}

/// Helper function to record a new rule creation in `AppState.reporter`, used in the router request
/// handlers for POST and PUT.
fn report_create_rule(state: &AppState, rule: &ChaosRule) {
    match state.reporter.try_write() {
        Ok(mut reporter) => reporter.create_rule(rule),
        Err(error) => tracing::trace!("failed to get write lock on analytics reporter: {error}"),
    }
}

/// Helper function to record a rule deletion in `AppState.reporter`, used in the router request
/// handlers for DELETE and PUT.
fn report_delete_rule(state: &AppState, rule: &ChaosRule) {
    match state.reporter.try_write() {
        Ok(mut reporter) => reporter.delete_rule(rule),
        Err(error) => tracing::trace!("failed to get write lock on analytics reporter: {error}"),
    }
}

/// Helper function to record deletion of all active rules in `AppState.reporter`, used in the
/// router request handler for DELETE.
fn report_clear_session_rules(state: &AppState) {
    match state.reporter.try_write() {
        Ok(mut reporter) => reporter.clear_session_rules(),
        Err(error) => tracing::trace!("failed to get write lock on analytics reporter: {error}"),
    }
}
