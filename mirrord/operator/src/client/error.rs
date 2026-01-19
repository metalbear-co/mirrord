use std::{fmt, num::ParseIntError};

pub use http::Error as HttpError;
use mirrord_auth::error::ApiKeyError;
use mirrord_kube::error::KubeApiError;
use mirrord_protocol_io::ProtocolError;
use thiserror::Error;
use tower::retry::backoff::InvalidBackoff;

use crate::crd::{NewOperatorFeature, kube_target::UnknownTargetType};

/// Operations performed on the operator via [`kube`] API.
#[derive(Debug)]
pub enum OperatorOperation {
    FindingOperator,
    FindingTarget,
    WebsocketConnection,
    CopyingTarget,
    GettingStatus,
    SessionManagement,
    ListingTargets,
    MongodbBranching,
    MysqlBranching,
    PgBranching,
    MultiClusterSession,
}

impl fmt::Display for OperatorOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let as_str = match self {
            Self::FindingOperator => "finding operator",
            Self::FindingTarget => "finding target",
            Self::WebsocketConnection => "creating a websocket connection",
            Self::CopyingTarget => "copying target",
            Self::GettingStatus => "getting status",
            Self::SessionManagement => "session management",
            Self::ListingTargets => "listing targets",
            Self::MongodbBranching => "mongodb branching",
            Self::MysqlBranching => "mysql branching",
            Self::PgBranching => "postgresql branching",
            Self::MultiClusterSession => "multi-cluster session",
        };

        f.write_str(as_str)
    }
}

#[derive(Debug, Error)]
pub enum OperatorApiError {
    #[error("failed to build a websocket connect request: {0}")]
    ConnectRequestBuildError(HttpError),

    #[error("failed to create Kubernetes client: {0}")]
    CreateKubeClient(KubeApiError),

    #[error("{operation} failed: {error}")]
    KubeError {
        error: kube::Error,
        operation: OperatorOperation,
    },

    #[error("mirrord operator {operator_version} does not support feature {feature}")]
    UnsupportedFeature {
        feature: NewOperatorFeature,
        operator_version: String,
    },

    #[error("{operation} failed with code {}: {}", status.code, status.reason)]
    StatusFailure {
        operation: OperatorOperation,
        status: Box<kube::core::Status>,
    },

    #[error("mirrord operator license expired")]
    NoLicense,

    #[error("failed to prepare client certificate: {0}")]
    ClientCertError(String),

    #[error("mirrord operator returned a target of unknown type: {}", .0 .0)]
    FetchedUnknownTargetType(#[from] UnknownTargetType),

    #[error("mirrord operator failed KubeApi operation: {0}")]
    KubeApi(#[from] KubeApiError),

    #[error(transparent)]
    ParseInt(#[from] ParseIntError),

    #[error("copied target failed: {}", message.as_deref().unwrap_or("reason unknown"))]
    CopiedTargetFailed { message: Option<String> },

    #[error("operation timed out: {}", operation)]
    OperationTimeout { operation: OperatorOperation },

    #[error("{operation} failed: {message}")]
    BranchCreationFailed {
        operation: OperatorOperation,
        message: String,
    },

    #[error(transparent)]
    InvalidBackoff(#[from] InvalidBackoff),

    #[error("protocol error: {0}")]
    ProtocolError(#[from] ProtocolError),

    #[error(transparent)]
    ApiKey(#[from] ApiKeyError),

    #[error(transparent)]
    SerdeJson(#[from] serde_json::Error),
}

pub type OperatorApiResult<T, E = OperatorApiError> = Result<T, E>;
