//! Defines the [`chaos_router`] [`Router`] for handling the `ChaosRule` CRUD.

use api::*;
use axum::{
    self, Router,
    middleware::{self},
    routing::{post, put},
};

use crate::ui::{chaos::error::ChaosApiError, server::AppState};

mod api;
mod error;

/// Alias for the return type of the chaos route handlers.
type ChaosResult<T> = Result<T, ChaosApiError>;

/// Routes for `/chaos/rules/{...}?token={mirrord-ui-token}`.
///
/// These routes are sort of a proxy between user requests and the intproxy session monitor server
/// that handles the `ChaosRule`s. Each mirrord session starts that server, and the user can send
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
