//! Config-wizard endpoints for the `mirrord ui` server, under `/api/v1`.
//!
//! Besides the `is-returning` onboarding state, the `up` routes serve the code of `mirrord up init`,
//! so the wizard asks the same questions and writes the same `mirrord-up.yaml`. Like `up init`,
//! they use the global `kube_context`, else the kubeconfig's current context. Contexts and
//! namespaces come from the shared `/api/v2/kube/*` API. All routes use the usual token auth.

use std::str::FromStr;

use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use mirrord_config::target::{Target, TargetType};
use mirrord_up::{
    PickedService, ServiceMode, SkeletonError, detect_ports, list_workloads_for_mode,
    render_skeleton,
};
use serde::{Deserialize, Serialize};

use crate::ui::{
    error::ApiError,
    server::{AppState, client_for_context, default_namespace},
};

/// Routes for `/api/v1/{...}` that back the config wizard page.
///
/// - `GET /is-returning`: whether the user has used the wizard before.
/// - `GET /up/modes`: [`ModesResponse`].
/// - `GET /up/workloads`: [`WorkloadsQuery`] → [`WorkloadsResponse`].
/// - `GET /up/ports`: [`PortsQuery`] → [`PortsResponse`].
/// - `POST /up/render`: [`RenderRequest`] → [`RenderResponse`].
pub(super) fn wizard_router() -> Router<AppState> {
    Router::new()
        .route("/is-returning", get(is_returning))
        .route("/up/modes", get(up_modes))
        .route("/up/workloads", get(up_workloads))
        .route("/up/ports", get(up_ports))
        .route("/up/render", post(up_render))
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

/// Flags the user as a returning wizard user on their first pass through the target picker.
async fn mark_returning(state: &AppState) {
    let mut user_data = state.user_data.lock().await;
    if !user_data.is_returning_wizard() {
        // ignore failures to persist
        let _ = user_data.update_is_returning_wizard().await;
    }
}

/// One mode, from the [`ServiceMode`] method of the same name.
#[derive(Debug, Serialize)]
struct ModeInfo {
    id: ServiceMode,
    label: &'static str,
    description: &'static str,
    recommended: bool,
    target_types: &'static [TargetType],
    targetless: bool,
    http_filter: bool,
    traffic_question: &'static str,
}

/// ### Response for `GET /api/v1/up/modes`
///
/// In display order; the first is preselected.
#[derive(Debug, Serialize)]
struct ModesResponse {
    modes: Vec<ModeInfo>,
}

async fn up_modes() -> Json<ModesResponse> {
    let modes = ServiceMode::OFFERED
        .into_iter()
        .map(|mode| ModeInfo {
            id: mode,
            label: mode.label(),
            description: mode.description(),
            recommended: mode.is_recommended(),
            target_types: mode.target_kinds(),
            targetless: mode.allows_targetless(),
            http_filter: mode.uses_http_filter(),
            traffic_question: mode.traffic_question(),
        })
        .collect();
    Json(ModesResponse { modes })
}

/// ### Query of `GET /api/v1/up/workloads`
#[derive(Debug, Deserialize)]
struct WorkloadsQuery {
    namespace: String,
    /// Decides the kinds listed.
    mode: ServiceMode,
}

/// ### Response for `GET /api/v1/up/workloads`
#[derive(Debug, Serialize)]
struct WorkloadsResponse {
    /// Target paths, like `pod/api-0/container/app`.
    workloads: Vec<String>,
}

/// Reaching the target picker means the user started the config flow, so this flags them as
/// returning.
async fn up_workloads(
    State(state): State<AppState>,
    Query(query): Query<WorkloadsQuery>,
) -> Result<Json<WorkloadsResponse>, ApiError> {
    mark_returning(&state).await;
    let client = client_for_context(None).await?;
    let workloads = list_workloads_for_mode(&client, &query.namespace, query.mode).await?;
    Ok(Json(WorkloadsResponse { workloads }))
}

/// ### Query of `GET /api/v1/up/ports`
#[derive(Debug, Deserialize)]
struct PortsQuery {
    namespace: String,
    /// A path from `GET /api/v1/up/workloads`.
    path: String,
}

/// ### Response for `GET /api/v1/up/ports`
#[derive(Debug, Serialize)]
struct PortsResponse {
    /// Sorted.
    ports: Vec<u16>,
}

async fn up_ports(Query(query): Query<PortsQuery>) -> Result<Json<PortsResponse>, ApiError> {
    let target =
        Target::from_str(&query.path).map_err(|_| ApiError::InvalidTarget(query.path.clone()))?;
    let client = client_for_context(None).await?;
    let ports = detect_ports(&client, &target, &query.namespace).await?;
    Ok(Json(PortsResponse {
        ports: ports.into_iter().collect(),
    }))
}

/// ### Body of `POST /api/v1/up/render`
#[derive(Debug, Deserialize)]
struct RenderRequest {
    services: Vec<PickedService>,
}

/// ### Response for `POST /api/v1/up/render`
#[derive(Debug, Serialize)]
struct RenderResponse {
    yaml: String,
}

async fn up_render(Json(request): Json<RenderRequest>) -> Result<Response, ApiError> {
    let namespace = default_namespace(None).await?;
    Ok(render_response(&request.services, &namespace))
}

/// `200` with the YAML, or `400` with why the picks make no valid file. Split out to test without
/// a cluster.
fn render_response(services: &[PickedService], default_namespace: &str) -> Response {
    match render_skeleton(services, default_namespace) {
        Ok(yaml) => Json(RenderResponse { yaml }).into_response(),
        Err(error) => {
            let status = match error {
                SkeletonError::Render(_) => StatusCode::INTERNAL_SERVER_ERROR,
                _ => StatusCode::BAD_REQUEST,
            };
            (
                status,
                Json(serde_json::json!({ "error": error.to_string() })),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use axum::body::to_bytes;

    use super::*;

    async fn body_json(response: Response) -> serde_json::Value {
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// The wire format the frontend reads.
    #[tokio::test]
    async fn modes_in_wire_format() {
        let Json(response) = up_modes().await;
        let modes = serde_json::to_value(response).unwrap();

        assert_eq!(modes["modes"][0]["id"], "split");
        assert_eq!(
            modes["modes"][2]["target_types"],
            serde_json::json!(["deployment", "statefulset"])
        );
    }

    /// Optional fields may be left out of the JSON.
    #[tokio::test]
    async fn render_returns_yaml() {
        let request: RenderRequest = serde_json::from_value(serde_json::json!({
            "services": [{
                "name": "api",
                "mode": "split",
                "target": { "path": "deployment/api", "namespace": "default" },
                "command": "npm start",
            }]
        }))
        .unwrap();

        let response = render_response(&request.services, "default");
        assert_eq!(response.status(), StatusCode::OK);
        let yaml = body_json(response).await["yaml"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(yaml.contains("path: deployment/api"), "{yaml}");
    }

    #[tokio::test]
    async fn render_rejects_invalid_picks() {
        let request: RenderRequest = serde_json::from_value(serde_json::json!({
            "services": [{
                "name": "api",
                "mode": "mirror",
                "target": null,
                "command": "npm start",
            }]
        }))
        .unwrap();

        let response = render_response(&request.services, "default");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            body_json(response).await["error"],
            "service `api`: `mirror` needs a target"
        );
    }
}
