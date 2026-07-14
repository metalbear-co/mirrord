//! Error type for the top-level `mirrord ui` route handlers that talk to Kubernetes.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use kube::config::KubeconfigError;
use mirrord_kube::error::KubeApiError;
use thiserror::Error;

/// Errors returned by the `mirrord ui` API handlers that read the kubeconfig or reach the cluster.
///
/// The HTTP status each variant maps to lives in the [`IntoResponse`] implementation: local
/// kubeconfig/client problems are `5xx`, a bad user-supplied context is `400`, and a failed request
/// to the cluster's API server is `502`.
#[derive(Debug, Error)]
pub(super) enum ApiError {
    /// Reading the merged kubeconfig (honouring `KUBECONFIG`) failed.
    #[error("failed to read kubeconfig: {0}")]
    ReadKubeconfig(#[source] KubeconfigError),

    /// Loading the requested context from the kubeconfig failed, e.g. the context does not exist.
    #[error("failed to load context {context:?}: {source}")]
    LoadContext {
        context: String,
        #[source]
        source: KubeconfigError,
    },

    /// Building the kube client from the resolved config failed.
    #[error("failed to initialize kube client: {0}")]
    KubeClient(#[from] kube::Error),

    /// A request to the cluster's API server failed.
    #[error("kube api request failed: {0}")]
    KubeApi(#[source] kube::Error),

    /// Looking up targets in the cluster (via the resource seeker) failed. Used by the wizard
    /// endpoints that enumerate namespaces and targets.
    #[error("kube resource lookup failed: {0}")]
    KubeResource(#[from] KubeApiError),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self {
            Self::ReadKubeconfig(_) | Self::KubeClient(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::LoadContext { .. } => StatusCode::BAD_REQUEST,
            Self::KubeApi(_) | Self::KubeResource(_) => StatusCode::BAD_GATEWAY,
        };

        (
            status,
            axum::Json(serde_json::json!({ "error": self.to_string() })),
        )
            .into_response()
    }
}
