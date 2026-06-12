use axum::{
    extract::rejection::JsonRejection,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use thiserror::Error;
use uuid::Uuid;

use crate::session_monitor::chaos::rules::ChaosRuleError;

#[derive(Debug, Error)]
pub(crate) enum ChaosApiError {
    #[error("chaos rule `{0}` not found")]
    RuleNotFound(Uuid),

    #[error(transparent)]
    Rule(#[from] ChaosRuleError),

    #[error(transparent)]
    Json(#[from] JsonRejection),

    #[error("chaos rule `{0}` was already in storage")]
    RuleAlreadyPresent(Uuid),
}

impl IntoResponse for ChaosApiError {
    fn into_response(self) -> Response {
        match self {
            Self::RuleNotFound(_) => (
                StatusCode::NOT_FOUND,
                format!("Could not find chaos rule: {self}"),
            )
                .into_response(),
            Self::Rule(_) => (
                StatusCode::BAD_REQUEST,
                format!("Invalid chaos rule operation: {self}"),
            )
                .into_response(),
            Self::Json(rejection) => rejection.into_response(),
            Self::RuleAlreadyPresent(_) => {
                (StatusCode::CONFLICT, format!("Unmodified: {self}")).into_response()
            }
        }
    }
}
