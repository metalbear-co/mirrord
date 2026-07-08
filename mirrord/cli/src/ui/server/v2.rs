//! # `mirrord ui` API v2
//!
//! A clean, versioned successor to the unversioned `/api/*` session-monitor routes. Those v1 routes
//! (and `/chaos/rules/*`) stay frozen because the separately-distributed browser extension calls
//! them; v2 is what the bundled frontend uses.
//!
//! The defining property of v2 is that it is **context/namespace-aware without any shared server
//! state**. Every cluster-touching endpoint takes an optional `?context=` (and, for operator
//! sessions, `?namespace=`), and each request is served against exactly that context. Two browser
//! tabs can therefore view two different clusters at once — the multi-tab hazard the v1 design (a
//! single background watcher on one context) can't express.
//!
//! Resource groups, all under `/api/v2` and behind the same [`token_auth`](super::token_auth):
//! - `local/*`    — sessions running on this host. Host-global (identical for every viewer); the
//!   frontend polls the list and each session is labelled with its own context and namespace.
//!   Always shown, never filtered by the selector.
//! - `operator/*` — cluster sessions from the operator. Fetched per-request for the selected
//!   context and filtered to the selected namespace; the upstream is itself a poll of the operator
//!   CRD, so a stateless per-context fetch is the natural fit.
//! - `kube/*`     — kubeconfig/cluster metadata used to populate the context and namespace pickers.

use axum::{
    Router,
    extract::{Query, State},
    routing::get,
};
use k8s_openapi::api::{authentication::v1::SelfSubjectReview, core::v1::Namespace};
use kube::{
    Api, Client,
    api::{ListParams, PostParams},
    config::Kubeconfig,
};
use mirrord_operator::crd::{MirrordOperatorCrd, OPERATOR_STATUS_NAME, SessionHttpFilter};
use serde::{Deserialize, Serialize};
use tracing::warn;

use super::{
    AppState, OperatorLockedPort, OperatorQueueSplits, OperatorSessionOwner,
    OperatorSessionSummary, OperatorSessionTarget, client_for_context, get_session, kill_session,
    list_sessions, session_events_sse,
};
use crate::ui::error::ApiError;

type V2Result<T> = Result<T, ApiError>;

/// Routes for `/api/v2`. State is supplied by the outer router's `with_state`, matching the other
/// route groups in [`build_router`](super::build_router).
pub(super) fn v2_router() -> Router<AppState> {
    Router::new()
        // Local sessions reuse the v1 handlers verbatim — the shapes already suit v2, and
        // `SessionInfo` now carries `context`.
        .route("/local/sessions", get(list_sessions))
        .route(
            "/local/sessions/{id}",
            get(get_session).delete(kill_session),
        )
        .route("/local/sessions/{id}/events", get(session_events_sse))
        .route("/operator/sessions", get(operator_sessions))
        .route("/kube/contexts", get(kube_contexts))
        .route("/kube/namespaces", get(kube_namespaces))
        .route("/kube/user", get(kube_user))
        // Target enumeration for the config wizard lives in the wizard module (it flags the
        // returning user), but is served here so the monitor and wizard share one kube API.
        .route("/kube/targets", get(crate::ui::wizard::list_targets))
        .route(
            "/kube/target-types",
            get(crate::ui::wizard::list_target_types),
        )
        .route("/token", get(token))
}

/// Returns a kube client for `context` (or the kubeconfig current context when `None`), caching it
/// in [`AppState::clients`] so the recurring operator-session polls don't rebuild TLS each time.
async fn cached_client(state: &AppState, context: Option<&str>) -> V2Result<Client> {
    let key = context.map(str::to_owned);
    if let Some(client) = state.clients.read().await.get(&key).cloned() {
        return Ok(client);
    }
    let client = client_for_context(context).await?;
    Ok(state
        .clients
        .write()
        .await
        .entry(key)
        .or_insert(client)
        .clone())
}

#[derive(Deserialize)]
struct ContextQuery {
    /// Context to act against. Absent means the kubeconfig's current context.
    context: Option<String>,
}

// ============================ operator (cluster) sessions ============================

#[derive(Deserialize)]
struct OperatorSessionsQuery {
    context: Option<String>,
    /// Namespace to filter cluster sessions to. Absent means all namespaces.
    namespace: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
enum OperatorStatus {
    Available,
    Unavailable,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OperatorSessionsResponse {
    /// Echoed back so a tab can confirm a response matches the context it currently has selected.
    context: Option<String>,
    status: OperatorStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    sessions: Vec<OperatorSession>,
}

/// One cluster session. This is [`OperatorSessionSummary`] minus `durationSecs`: age is derived
/// client-side from `createdAt`, so there's no need to also ship a snapshot duration.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OperatorSession {
    id: String,
    key: String,
    namespace: String,
    owner: OperatorSessionOwner,
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<OperatorSessionTarget>,
    created_at: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    locked_ports: Vec<OperatorLockedPort>,
    queue_splits: OperatorQueueSplits,
    #[serde(skip_serializing_if = "Option::is_none")]
    http_filter: Option<SessionHttpFilter>,
}

impl From<OperatorSessionSummary> for OperatorSession {
    fn from(summary: OperatorSessionSummary) -> Self {
        Self {
            id: summary.id,
            key: summary.key,
            namespace: summary.namespace,
            owner: summary.owner,
            target: summary.target,
            created_at: summary.created_at,
            locked_ports: summary.locked_ports,
            queue_splits: summary.queue_splits,
            http_filter: summary.http_filter,
        }
    }
}

/// Fetches the operator's live sessions for `context` in one request. Returns a human-readable
/// reason when the context is unreachable or the operator isn't installed.
async fn fetch_operator(
    state: &AppState,
    context: Option<&str>,
) -> Result<Vec<OperatorSessionSummary>, String> {
    let client = cached_client(state, context)
        .await
        .map_err(|err| format!("kube client init failed: {err}"))?;
    let api: Api<MirrordOperatorCrd> = Api::all(client);
    let operator = api
        .get(OPERATOR_STATUS_NAME)
        .await
        .map_err(|err| format!("operator not available: {err}"))?;

    Ok(operator
        .status
        .as_ref()
        .map(|status| status.sessions.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(OperatorSessionSummary::from_session)
        .collect())
}

async fn operator_sessions(
    State(state): State<AppState>,
    Query(query): Query<OperatorSessionsQuery>,
) -> axum::Json<OperatorSessionsResponse> {
    let response = match fetch_operator(&state, query.context.as_deref()).await {
        Ok(sessions) => {
            let sessions = sessions
                .into_iter()
                .filter(|session| {
                    query
                        .namespace
                        .as_deref()
                        .is_none_or(|namespace| session.namespace == namespace)
                })
                .map(OperatorSession::from)
                .collect();
            OperatorSessionsResponse {
                context: query.context,
                status: OperatorStatus::Available,
                reason: None,
                sessions,
            }
        }
        Err(reason) => {
            warn!(context = ?query.context, "{reason}");
            OperatorSessionsResponse {
                context: query.context,
                status: OperatorStatus::Unavailable,
                reason: Some(reason),
                sessions: Vec::new(),
            }
        }
    };
    axum::Json(response)
}

// ============================ kube metadata ============================

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ContextsResponse {
    current: Option<String>,
    contexts: Vec<ContextEntry>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ContextEntry {
    name: String,
    /// The context's configured default namespace (its `context.namespace` in the kubeconfig). The
    /// frontend defaults its namespace filter to this, so no extra call is needed to learn it.
    namespace: Option<String>,
}

/// Lists kube contexts and each one's default namespace, straight from the merged kubeconfig — no
/// cluster access, so it works even when no cluster is reachable.
async fn kube_contexts() -> V2Result<axum::Json<ContextsResponse>> {
    let kubeconfig = Kubeconfig::read().map_err(ApiError::ReadKubeconfig)?;
    let contexts = kubeconfig
        .contexts
        .iter()
        .map(|named| ContextEntry {
            name: named.name.clone(),
            namespace: named
                .context
                .as_ref()
                .and_then(|context| context.namespace.clone()),
        })
        .collect();
    Ok(axum::Json(ContextsResponse {
        current: kubeconfig.current_context,
        contexts,
    }))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NamespacesResponse {
    context: Option<String>,
    namespaces: Vec<String>,
}

/// Lists namespaces visible in a context. Queries the cluster API server, so the context must be
/// reachable and the user authorized to list namespaces.
async fn kube_namespaces(
    State(state): State<AppState>,
    Query(query): Query<ContextQuery>,
) -> V2Result<axum::Json<NamespacesResponse>> {
    let client = cached_client(&state, query.context.as_deref()).await?;
    let api: Api<Namespace> = Api::all(client);
    let namespaces = api
        .list(&ListParams::default())
        .await
        .map_err(ApiError::KubeApi)?;
    Ok(axum::Json(NamespacesResponse {
        context: query.context,
        namespaces: namespaces
            .iter()
            .filter_map(|namespace| namespace.metadata.name.clone())
            .collect(),
    }))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UserResponse {
    username: Option<String>,
}

/// The current k8s username in a context, used to split "your" cluster sessions from the team's.
/// `null` when it can't be resolved; the frontend degrades to showing everything as team sessions.
async fn kube_user(
    State(state): State<AppState>,
    Query(query): Query<ContextQuery>,
) -> axum::Json<UserResponse> {
    let username = resolve_username(&state, query.context.as_deref()).await;
    axum::Json(UserResponse { username })
}

async fn resolve_username(state: &AppState, context: Option<&str>) -> Option<String> {
    let client = cached_client(state, context).await.ok()?;
    let api: Api<SelfSubjectReview> = Api::all(client);
    let review = api
        .create(&PostParams::default(), &SelfSubjectReview::default())
        .await
        .ok()?;
    review
        .status
        .and_then(|status| status.user_info)
        .and_then(|user| user.username)
}

#[derive(Serialize)]
struct TokenResponse {
    token: String,
}

async fn token(State(state): State<AppState>) -> axum::Json<TokenResponse> {
    axum::Json(TokenResponse {
        token: state.token.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The v2 session shape is camelCase and omits `durationSecs` (age is derived from
    /// `createdAt`).
    #[test]
    fn operator_session_serializes_camelcase_without_duration() {
        let summary = OperatorSessionSummary {
            id: "cr-1".to_owned(),
            key: "k".to_owned(),
            namespace: "team-a".to_owned(),
            owner: OperatorSessionOwner {
                username: "alice".to_owned(),
                k8s_username: "alice@ex".to_owned(),
            },
            target: None,
            created_at: "2020-01-01T00:00:00Z".to_owned(),
            duration_secs: 42,
            locked_ports: Vec::new(),
            queue_splits: OperatorQueueSplits::default(),
            http_filter: None,
        };

        let json = serde_json::to_value(OperatorSession::from(summary)).unwrap();
        assert_eq!(
            json.get("namespace").and_then(|v| v.as_str()),
            Some("team-a")
        );
        assert_eq!(
            json.get("createdAt").and_then(|v| v.as_str()),
            Some("2020-01-01T00:00:00Z")
        );
        assert!(json.get("durationSecs").is_none());
        assert!(json.get("duration_secs").is_none());
    }

    /// `available`/`unavailable` are the only two states v2 emits.
    #[test]
    fn operator_status_serializes_lowercase() {
        assert_eq!(
            serde_json::to_value(OperatorStatus::Available).unwrap(),
            serde_json::json!("available")
        );
        assert_eq!(
            serde_json::to_value(OperatorStatus::Unavailable).unwrap(),
            serde_json::json!("unavailable")
        );
    }
}
