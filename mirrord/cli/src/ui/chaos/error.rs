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

    #[error("chaos rule not found")]
    ChaosRuleNotFound,

    #[error(transparent)]
    SessionMonitor(SessionError),
}

impl From<SessionError> for ChaosApiError {
    fn from(error: SessionError) -> Self {
        match error {
            SessionError::BadStatus(StatusCode::NOT_FOUND) => Self::ChaosRuleNotFound,
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
            Self::ChaosRuleNotFound => (
                StatusCode::NOT_FOUND,
                format!("Could not find chaos rule: {self}"),
            )
                .into_response(),
            Self::SessionMonitor(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Something went wrong: {self}"),
            )
                .into_response(),
        }
    }
}
