//! Config-wizard endpoints for the `mirrord ui` server.
//!
//! Only the wizard-specific `is-returning` onboarding state lives here, under `/api/v1`. The
//! cluster reads the wizard's target picker needs (namespaces, targets, target types) are served
//! under the shared `/api/v2/kube/*` API. All routes are gated by the same token auth as every
//! other route.

use axum::{Router, extract::State, routing::get};

use crate::ui::server::AppState;

/// Routes for `/api/v1/{...}` that back the config wizard page.
///
/// - `GET /is-returning`: whether the user has used the wizard before.
pub(super) fn wizard_router() -> Router<AppState> {
    Router::new().route("/is-returning", get(is_returning))
}

/// Returns whether the user has used the wizard enough times to be considered returning, as a bare
/// `"true"`/`"false"` string (the shape the frontend expects).
async fn is_returning(State(state): State<AppState>) -> String {
    state
        .user_data
        .lock()
        .await
        .is_returning_wizard()
        .to_string()
}
