use anyhow::anyhow;
use axum::{
    Extension, Json, Router,
    extract::{Path, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{post, put},
};
use mirrord_intproxy::session_monitor::chaos::{
    ChaosRuleList,
    rules::{ChaosRule, ChaosRuleRequest, ChaosSelectorRequest},
};
use mirrord_session_monitor_client::SessionClient;
use serde_json::Value;
use uuid::Uuid;

use crate::ui::AppState;

/*
POST /chaos/rules/{session_id}: create rule, return rule object with assigned ID
GET /chaos/rules/{session_id}: list active rules for session
GET /chaos/rules/{session_id}/{rule_id}: get specific rule
PUT /chaos/rules/{session_id}/{rule_id}: update rule
DELETE /chaos/rules/{session_id}/{rule_id}: delete rule
DELETE /chaos/rules/{session_id}: clear all rules for session*/

const BASE_INTPROXY_CHAOS_ROUTE: &str = "/chaos/rules";

// TODO(alex): Ok, so this works sort of like this:
// Some random runs `mirrord ui`, it starts up the axum server (let's say the address is
// `ui:localhost/chaos`), and it's also running an axum server in the intproxy (address
// `monitor:localhost/chaos`), something called `session_monitor` (that's your keyword to search).
//
// The random wants to go mid (I mean, wants to create a rule), so they click some button in the ui
// that sends a POST request to `POST ui:localhost/chaos/1234`, it hits the route you're seeing
// here, and we use a `reqwest::Client` that's in the `TrackedSession` that's in `AppState` that's
// in this codebase that's in my computer that's in ...
//
// This `Client` is used to send a reqwest (lol) to `monitor:localhost/chaos/1234`, and in there ...
// (go to the file `chaos.rs` in `/intproxy`).
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

async fn get_session_client_middleware(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    mut request: Request,
    next: Next,
) -> ChaosResult<Response> {
    let sessions = state.sessions.read().await;

    match sessions.get(&session_id.to_string()) {
        Some(session) => {
            request.extensions_mut().insert(session.client.clone());
            Ok(next.run(request).await)
        }
        None => Err(anyhow!("lol"))?,
    }
}

#[derive(Debug)]
struct ApiError(anyhow::Error);

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

type ChaosResult<T> = Result<T, ApiError>;

async fn post_create_rule(
    Path(session_id): Path<String>,
    Extension(client): Extension<SessionClient>,
    Json(lol): Json<Value>,
) -> ChaosResult<()> {
    let new_rule = ChaosRuleRequest {
        name: Some("knuckles".to_string()),
        priority: Some(100),
        effect:
            mirrord_intproxy::session_monitor::chaos::rules::ChaosEffectRequest::ConnectionError {
                error_type: "refused".to_string(),
                after_ms: Some(0),
            },
        // selector: ChaosSelectorRequest::tcp_port(443, Some(100)),
        selector: ChaosSelectorRequest::name(),
    };

    client
        .post(format!("{BASE_INTPROXY_CHAOS_ROUTE}/{session_id}"))
        .json(&new_rule)
        .send()
        .await
        .inspect_err(|fail| println!("{fail:?}"))?;

    Ok(())
}

async fn get_list_active_rules_for_session(
    Path(session_id): Path<String>,
    Extension(client): Extension<SessionClient>,
) -> ChaosResult<Json<ChaosRuleList>> {
    let response = client
        .get(format!("{BASE_INTPROXY_CHAOS_ROUTE}/{session_id}"))
        .send()
        .await?
        .json::<ChaosRuleList>()
        .await?;

    println!("{response:?} yay response");

    Ok(Json(response))
}

async fn delete_clear_session_rules(
    Path(session_id): Path<String>,
    Extension(client): Extension<SessionClient>,
) -> ChaosResult<()> {
    client
        .delete(format!("{BASE_INTPROXY_CHAOS_ROUTE}/{session_id}"))
        .send()
        .await?;

    Ok(())
}

async fn put_update_rule(
    Path((session_id, rule_id)): Path<(String, Uuid)>,
    Extension(client): Extension<SessionClient>,
    Json(updated_rule): Json<ChaosRuleRequest>,
) -> ChaosResult<()> {
    client
        .post(format!(
            "{BASE_INTPROXY_CHAOS_ROUTE}/{session_id}/{rule_id}"
        ))
        .json(&updated_rule)
        .send()
        .await?;

    Ok(())
}

async fn delete_rule(
    Path((session_id, rule_id)): Path<(String, Uuid)>,
    Extension(client): Extension<SessionClient>,
) -> ChaosResult<()> {
    client
        .delete(format!(
            "{BASE_INTPROXY_CHAOS_ROUTE}/{session_id}/{rule_id}"
        ))
        .send()
        .await?;

    Ok(())
}

async fn get_rule(
    Path((session_id, rule_id)): Path<(String, Uuid)>,
    Extension(client): Extension<SessionClient>,
) -> ChaosResult<Json<ChaosRule>> {
    let response = client
        .get(format!(
            "{BASE_INTPROXY_CHAOS_ROUTE}/{session_id}/{rule_id}"
        ))
        .send()
        .await?
        .json::<ChaosRule>()
        .await?;

    println!("{response:?} yay response");

    Ok(Json(response))
}
