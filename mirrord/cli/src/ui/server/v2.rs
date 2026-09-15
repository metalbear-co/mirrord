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

use std::collections::BTreeMap;

use axum::{
    Router,
    extract::{Path, Query, State},
    routing::get,
};
use k8s_openapi::api::{authentication::v1::SelfSubjectReview, core::v1::Namespace};
use kube::{
    Api, Client,
    api::{ListParams, PostParams},
    config::Kubeconfig,
};
use mirrord_operator::crd::{
    MirrordOperatorCrd, OPERATOR_STATUS_NAME, SessionHttpFilter,
    preview::{
        PreviewPodLogs,
        view::{PreviewEnv, PreviewEnvStatus, PreviewMessageKind},
    },
};
use serde::{Deserialize, Serialize};
use tracing::warn;

use super::{
    AppState, OperatorLicense, OperatorLockedPort, OperatorPreviewSession, OperatorQueueSplits,
    OperatorSessionOwner, OperatorSessionSummary, OperatorSessionTarget, UiResult,
    client_for_context, get_session, kill_session, list_sessions, session_events_sse,
};
use crate::ui::error::ApiError;

mod targets;

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
        .route("/operator/previews/{id}", get(operator_preview_detail))
        .route("/operator/license", get(operator_license))
        .route("/kube/contexts", get(kube_contexts))
        .route("/kube/namespaces", get(kube_namespaces))
        .route("/kube/user", get(kube_user))
        .route("/kube/targets", get(targets::list_targets))
        .route("/kube/target-types", get(targets::list_target_types))
        .route("/token", get(token))
}

/// Returns a kube client for `context` (or the kubeconfig current context when `None`), caching it
/// in [`AppState::clients`] so the recurring operator-session polls don't rebuild TLS each time.
/// Callers should [`evict_client`] when a request made with the returned client fails, so a broken
/// client is rebuilt rather than reused.
async fn cached_client(state: &AppState, context: Option<&str>) -> UiResult<Client> {
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

/// Drops the cached client for `context` so the next [`cached_client`] rebuilds it from the
/// kubeconfig. kube-rs refreshes short-lived credentials on its own, but a cached client can still
/// go stale for other reasons (a rotated CA, a moved API server, a failing auth-exec plugin), and
/// rebuilding is the cheap way to recover — the alternative is serving a dead client until restart.
async fn evict_client(state: &AppState, context: Option<&str>) {
    state
        .clients
        .write()
        .await
        .remove(&context.map(str::to_owned));
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
    /// Preview environments info.
    /// For backward compatibility, preview session info is also chained into `sessions` where
    /// released clients read. But new info of preview should be added in `preview_sessions`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    preview_sessions: Vec<OperatorPreviewSession>,
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

struct OperatorStatusSummary {
    sessions: Vec<OperatorSessionSummary>,
    preview_sessions: Vec<OperatorPreviewSession>,
    license: OperatorLicense,
}

/// Fetches the operator's live sessions and license for `context` in one request. Returns a
/// human-readable reason when the context is unreachable or the operator isn't installed.
async fn fetch_operator(
    state: &AppState,
    context: Option<&str>,
) -> Result<OperatorStatusSummary, String> {
    let client = cached_client(state, context)
        .await
        .map_err(|err| format!("kube client init failed: {err}"))?;
    let api: Api<MirrordOperatorCrd> = Api::all(client);
    let operator = match api.get(OPERATOR_STATUS_NAME).await {
        Ok(operator) => operator,
        Err(err) => {
            // The cached client may be the cause; drop it so the next poll rebuilds it.
            evict_client(state, context).await;
            return Err(format!("operator not available: {err}"));
        }
    };

    let license = OperatorLicense {
        fingerprint: operator.spec.license.fingerprint.clone(),
        organization: operator.spec.license.organization.clone(),
    };
    let status = operator.status.as_ref();
    let sessions = status
        .map(|status| status.sessions.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(OperatorSessionSummary::from_session)
        .collect();
    let preview_sessions = status
        .map(|status| status.preview_sessions.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(OperatorPreviewSession::from_preview)
        .collect();

    Ok(OperatorStatusSummary {
        sessions,
        preview_sessions,
        license,
    })
}

async fn operator_sessions(
    State(state): State<AppState>,
    Query(query): Query<OperatorSessionsQuery>,
) -> axum::Json<OperatorSessionsResponse> {
    let response = match fetch_operator(&state, query.context.as_deref()).await {
        Ok(snapshot) => {
            let in_namespace = |namespace: &str| {
                query
                    .namespace
                    .as_deref()
                    .is_none_or(|selected| namespace == selected)
            };

            let sessions = snapshot
                .sessions
                .into_iter()
                .filter(|session| in_namespace(&session.namespace))
                .map(OperatorSession::from)
                .collect();
            let preview_sessions = snapshot
                .preview_sessions
                .into_iter()
                .filter(|preview| in_namespace(&preview.namespace))
                .collect();

            OperatorSessionsResponse {
                context: query.context,
                status: OperatorStatus::Available,
                reason: None,
                sessions,
                preview_sessions,
            }
        }
        Err(reason) => {
            warn!(context = ?query.context, "{reason}");
            OperatorSessionsResponse {
                context: query.context,
                status: OperatorStatus::Unavailable,
                reason: Some(reason),
                sessions: Vec::new(),
                preview_sessions: Vec::new(),
            }
        }
    };
    axum::Json(response)
}

/// Query for [`operator_preview_detail`].
#[derive(Deserialize)]
struct PreviewDetailQuery {
    context: Option<String>,
    /// Namespace the preview lives in; the id is only unique within one.
    namespace: Option<String>,
    /// Whether to also tail the preview's pods. Off by default: each read costs a log fetch per
    /// pod, fanned out to every workload cluster, so it is worth paying only for a preview whose
    /// phase the user is actually trying to explain.
    #[serde(default)]
    logs: bool,
}

/// Severity of a [`PreviewDetailResponse::message`], lower-cased for the frontend like
/// [`OperatorPreviewPhase`](super::OperatorPreviewPhase).
#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
enum PreviewMessageSeverity {
    Failure,
    Degraded,
    Unknown,
}

impl From<PreviewMessageKind> for PreviewMessageSeverity {
    fn from(kind: PreviewMessageKind) -> Self {
        match kind {
            PreviewMessageKind::Failure => Self::Failure,
            PreviewMessageKind::Degraded => Self::Degraded,
            PreviewMessageKind::Unknown => Self::Unknown,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PreviewMessageView {
    severity: PreviewMessageSeverity,
    text: String,
}

/// Why one preview is in the phase the session list reports for it.
///
/// Served on demand rather than folded into `operator/sessions`: the operator answers
/// `/previews` by joining the primary's CRs with a live read of every workload cluster, so
/// putting it on the five-second poll would fan out to the whole fleet for every open tab.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PreviewDetailResponse {
    /// The name the operator addresses this preview by, which the session list does not carry
    /// (it identifies previews by uid).
    name: String,
    image: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<PreviewMessageView>,
    /// Per-workload-cluster phase, empty on a single-cluster operator.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    clusters: BTreeMap<String, String>,
    /// Populated only when the request asked for logs.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    logs: Vec<PreviewPodLogs>,
    /// Why the log read produced nothing, when one was asked for and failed. An operator that
    /// predates the `logs` subresource answers 404 here, so this must never fail the response:
    /// the message and per-cluster phases are the half a client can still act on, and they are
    /// most wanted on exactly the failed previews that ask for logs.
    #[serde(skip_serializing_if = "Option::is_none")]
    logs_error: Option<String>,
}

impl PreviewDetailResponse {
    /// Flattens the operator's view into the shape the frontend reads, lower-casing the enums
    /// it renders so they match the phase strings the session list already uses.
    fn from_view(
        preview: PreviewEnv,
        logs: Vec<PreviewPodLogs>,
        logs_error: Option<String>,
    ) -> Self {
        let status = preview.status;

        Self {
            name: preview.metadata.name.unwrap_or_default(),
            image: preview.spec.image,
            message: status
                .as_ref()
                .and_then(|status| status.message.as_ref())
                .map(|message| PreviewMessageView {
                    severity: message.kind.into(),
                    text: message.text.clone(),
                }),
            clusters: status
                .map(|status| {
                    status
                        .clusters
                        .into_iter()
                        .map(|(cluster, status)| (cluster, status.phase.to_string().to_lowercase()))
                        .collect()
                })
                .unwrap_or_default(),
            logs,
            logs_error,
        }
    }
}

/// Serves `GET /operator/previews/{id}`, where `id` is the uid the session list reports.
///
/// Resolves that uid against the operator's `previews` view, which is the only place the
/// failure message, per-cluster phases and image live - the operator CRD status the session
/// list is built from carries none of them.
async fn operator_preview_detail(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<PreviewDetailQuery>,
) -> UiResult<axum::Json<PreviewDetailResponse>> {
    let client = cached_client(&state, query.context.as_deref()).await?;

    let api: Api<PreviewEnv> = match query.namespace.as_deref() {
        Some(namespace) => Api::namespaced(client.clone(), namespace),
        None => Api::all(client.clone()),
    };

    let previews = match api.list(&ListParams::default()).await {
        Ok(previews) => previews,
        Err(error) => {
            // A cached client that has gone stale looks exactly like an unreachable operator
            // here, so let the next request rebuild it.
            evict_client(&state, query.context.as_deref()).await;
            return Err(ApiError::KubeApi(error));
        }
    };

    let preview = previews
        .items
        .into_iter()
        .find(|preview| preview.metadata.uid.as_deref() == Some(id.as_str()))
        .ok_or_else(|| ApiError::NotFound {
            kind: "preview environment",
            id: id.clone(),
        })?;

    let (logs, logs_error) = if query.logs {
        // Addressed through the preview's OWN namespace rather than `api`, which is
        // namespace-less when the frontend has no namespace selected - a subresource is only
        // reachable under the namespaced path.
        let namespaced = Api::<PreviewEnv>::namespaced(
            client,
            preview.metadata.namespace.as_deref().unwrap_or_default(),
        );

        match namespaced
            .get_subresource("logs", preview.metadata.name.as_deref().unwrap_or_default())
            .await
        {
            Ok(view) => (
                view.status
                    .map(|status: PreviewEnvStatus| status.logs)
                    .unwrap_or_default(),
                None,
            ),
            Err(error) => (Vec::new(), Some(error.to_string())),
        }
    } else {
        (Vec::new(), None)
    };

    Ok(axum::Json(PreviewDetailResponse::from_view(
        preview, logs, logs_error,
    )))
}

/// The operator license for a context, or `null` when the operator is unreachable. Split out from
/// the session list because the frontend only needs it once per context, to attribute ui usage to
/// the licensed organization in analytics — not on every session poll.
async fn operator_license(
    State(state): State<AppState>,
    Query(query): Query<ContextQuery>,
) -> axum::Json<Option<OperatorLicense>> {
    let license = fetch_operator(&state, query.context.as_deref())
        .await
        .ok()
        .map(|summary| summary.license);
    axum::Json(license)
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
async fn kube_contexts() -> UiResult<axum::Json<ContextsResponse>> {
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
) -> UiResult<axum::Json<NamespacesResponse>> {
    let client = cached_client(&state, query.context.as_deref()).await?;
    let api: Api<Namespace> = Api::all(client);
    let namespaces = match api.list(&ListParams::default()).await {
        Ok(namespaces) => namespaces,
        Err(err) => {
            evict_client(&state, query.context.as_deref()).await;
            return Err(ApiError::KubeApi(err));
        }
    };
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
    let review = match api
        .create(&PostParams::default(), &SelfSubjectReview::default())
        .await
    {
        Ok(review) => review,
        Err(_) => {
            evict_client(state, context).await;
            return None;
        }
    };
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
    use mirrord_operator::crd::{
        preview::{
            PreviewSessionPhase,
            view::{PreviewClusterStatus, PreviewEnvPhase, PreviewEnvSpec, PreviewMessage},
        },
        session::{KubeResourceTarget, SessionTarget},
    };

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

    fn preview_view(status: Option<PreviewEnvStatus>) -> PreviewEnv {
        let mut view = PreviewEnv::new(
            "preview-abc",
            PreviewEnvSpec {
                key: "my-key".to_owned(),
                target: SessionTarget::KubeResource(KubeResourceTarget {
                    api_version: "apps/v1".to_owned(),
                    kind: "Deployment".to_owned(),
                    name: "api".to_owned(),
                    container: "api".to_owned(),
                }),
                image: "ghcr.io/acme/api:1".to_owned(),
            },
        );
        view.metadata.uid = Some("uid-1".to_owned());
        view.status = status;
        view
    }

    /// The frontend keys its presentation off `severity`, so the operator's message kind has to
    /// reach it lower-cased like every other enum v2 serves.
    #[test]
    fn preview_message_severity_serializes_lowercase() {
        let response = PreviewDetailResponse::from_view(
            preview_view(Some(PreviewEnvStatus {
                phase: None,
                message: Some(PreviewMessage {
                    kind: PreviewMessageKind::Degraded,
                    text: "replicas disabled".to_owned(),
                }),
                clusters: Default::default(),
                logs: Vec::new(),
            })),
            Vec::new(),
            None,
        );

        let json = serde_json::to_value(response).unwrap();
        assert_eq!(
            json.pointer("/message/severity"),
            Some(&serde_json::json!("degraded"))
        );
        assert_eq!(
            json.pointer("/message/text").and_then(|v| v.as_str()),
            Some("replicas disabled")
        );
    }

    /// Per-cluster phases are rendered next to the session list's own phase strings, which are
    /// lower-case; the view serves them capitalised.
    #[test]
    fn preview_cluster_phases_are_lowercased() {
        let clusters = [
            (
                "eu".to_owned(),
                PreviewClusterStatus {
                    phase: PreviewEnvPhase::Active(PreviewSessionPhase::Ready),
                },
            ),
            (
                "us".to_owned(),
                PreviewClusterStatus {
                    phase: PreviewEnvPhase::Unreachable,
                },
            ),
        ]
        .into_iter()
        .collect();

        let response = PreviewDetailResponse::from_view(
            preview_view(Some(PreviewEnvStatus {
                phase: None,
                message: None,
                clusters,
                logs: Vec::new(),
            })),
            Vec::new(),
            None,
        );

        assert_eq!(
            response.clusters.get("eu").map(String::as_str),
            Some("ready")
        );
        assert_eq!(
            response.clusters.get("us").map(String::as_str),
            Some("unreachable")
        );
    }

    /// The id the session list reports is the uid, but the operator addresses the preview by
    /// name - so the response has to carry the name for anything that follows up on it.
    #[test]
    fn preview_detail_reports_the_name_not_the_uid() {
        let response = PreviewDetailResponse::from_view(preview_view(None), Vec::new(), None);

        assert_eq!(response.name, "preview-abc");
        assert_eq!(response.image, "ghcr.io/acme/api:1");
    }

    /// A status-less view must still answer, so a preview the operator has not reported on yet
    /// renders as an ordinary entry rather than an error.
    #[test]
    fn preview_without_status_omits_message_and_clusters() {
        let json = serde_json::to_value(PreviewDetailResponse::from_view(
            preview_view(None),
            Vec::new(),
            None,
        ))
        .unwrap();

        assert!(json.get("message").is_none());
        assert!(json.get("clusters").is_none());
        assert!(json.get("logs").is_none());
    }

    /// An operator without the `logs` subresource must still answer with the message and
    /// per-cluster phases - they are the half the client can act on, and a failed preview is
    /// exactly what asks for logs.
    #[test]
    fn a_failed_log_read_still_reports_the_message() {
        let response = PreviewDetailResponse::from_view(
            preview_view(Some(PreviewEnvStatus {
                phase: None,
                message: Some(PreviewMessage {
                    kind: PreviewMessageKind::Failure,
                    text: "keystore missing".to_owned(),
                }),
                clusters: Default::default(),
                logs: Vec::new(),
            })),
            Vec::new(),
            Some("404 page not found".to_owned()),
        );

        let json = serde_json::to_value(response).unwrap();
        assert_eq!(
            json.pointer("/message/text").and_then(|v| v.as_str()),
            Some("keystore missing")
        );
        assert_eq!(
            json.get("logsError").and_then(|v| v.as_str()),
            Some("404 page not found")
        );
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
