//! Defines the error types for the chaos rules route handlers.
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use mirrord_session_monitor_client::SessionError;
use thiserror::Error;

#[derive(Debug, Error)]
pub(super) enum ChaosApiError {
    #[error("session `{0}` not found")]
    SessionNotFound(String),

    /// We got a response error from the session monitor chaos api (intproxy), e.g. we made a
    /// request with `rule_id={some-uid}`, and it returned `404`, so we use this to upstream the
    /// error.
    #[error("session monitor returned status {status}: {body}")]
    Upstream { status: StatusCode, body: String },

    /// Something went wrong communicating with the session monitor.
    #[error(transparent)]
    SessionMonitor(SessionError),
}

impl From<SessionError> for ChaosApiError {
    fn from(error: SessionError) -> Self {
        match error {
            SessionError::Upstream { status, body } => Self::Upstream { status, body },
            other => Self::SessionMonitor(other),
        }
    }
}

impl IntoResponse for ChaosApiError {
    fn into_response(self) -> Response {
        match self {
            Self::SessionNotFound(_) => (
                StatusCode::NOT_FOUND,
                format!("Could not find session: {self}"),
            )
                .into_response(),
            Self::Upstream { status, body } => (status, body).into_response(),
            Self::SessionMonitor(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Something went wrong: {self}"),
            )
                .into_response(),
        }
    }
}
