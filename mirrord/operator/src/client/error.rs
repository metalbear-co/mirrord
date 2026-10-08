use std::{fmt, num::ParseIntError};

pub use http::Error as HttpError;
use mirrord_auth::error::ApiKeyError;
use mirrord_config::config::ConfigError;
use mirrord_kube::error::KubeApiError;
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
    DbBranching,
    PgBranching,
    MysqlBranching,
    MongodbBranching,
    PreparingClientCertificate,
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
            Self::DbBranching => "database branching",
            Self::PgBranching => "PostgreSQL branching",
            Self::MysqlBranching => "MySQL branching",
            Self::MongodbBranching => "MongoDB branching",
            Self::PreparingClientCertificate => "preparing client certificate",
        };

        f.write_str(as_str)
    }
}

#[derive(Debug, Error)]
pub enum OperatorApiError {
    #[error("failed to build a websocket connect request: {0}")]
    ConnectRequestBuildError(HttpError),

    #[error("configured baggage is not a valid HTTP header value: {0}")]
    InvalidBaggageHeader(#[from] http::header::InvalidHeaderValue),

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

    /// The operator's version knows about the feature but the current deployment doesn't advertise
    /// it, meaning it's disabled in the operator's configuration. Unlike
    /// [`Self::UnsupportedFeature`] this is not fixed by upgrading; the cluster admin has to
    /// enable it.
    #[error("feature {feature} is not enabled on this mirrord operator")]
    FeatureDisabled { feature: NewOperatorFeature },

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

    #[error("failed to resolve target: {0}")]
    TargetResolutionFailed(String),

    #[error("unsupported target configuration: {0}")]
    UnsupportedTargetConfig(String),

    #[error(transparent)]
    InvalidBackoff(#[from] InvalidBackoff),

    #[error(transparent)]
    ApiKey(#[from] ApiKeyError),

    #[error(transparent)]
    SerdeJson(#[from] serde_json::Error),

    #[error("failed to create credential secret: {0}")]
    CredentialSecretCreation(String),

    /// The operator could not resolve `feature.db_branches` against the target's
    /// `MirrordSplitConfig`. The message comes from the operator and names the workload, the
    /// namespace, and the configs it looked at.
    #[error("failed to resolve db_branches from the target's MirrordSplitConfig: {0}")]
    SplitConfigDbBranches(String),

    /// A resolved `dbBranches` entry does not parse with this CLI's config version.
    #[error(
        "dbBranches entry `{id}` on MirrordSplitConfig `{split_configs}` uses a setting this \
         mirrord version does not know: {error}"
    )]
    SplitConfigDbBranchEntry {
        id: String,
        split_configs: String,
        error: String,
    },

    /// The entries resolved from the target's `MirrordSplitConfig` fail a check an inline
    /// `feature.db_branches` fails at config load, such as a connection variable that
    /// `feature.env.override` also sets.
    #[error(
        "the db_branches resolved from the target's MirrordSplitConfig do not fit this config: {0}"
    )]
    ResolvedDbBranchesInvalid(#[source] ConfigError),

    /// Attaching to a branch another session created under the same key, from an entry whose
    /// copy mode differs from the one the branch was created with.
    #[error(
        "branch `{branch_id}` exists with copy mode \"{existing_mode}\", this service asked for \
         \"{requested_mode}\""
    )]
    BranchCopyModeMismatch {
        branch_id: String,
        existing_mode: String,
        requested_mode: String,
        /// The workload whose session created the branch.
        creator: String,
    },

    #[error("failed to create preview secret mounts: {0}")]
    PreviewSecretMountCreation(String),

    #[error("failed to read branch migrations from {path}: {error}")]
    MigrationsRead { path: String, error: String },

    #[error("migrations archive {path} too large: {size}/{limit} bytes")]
    MigrationsTooLarge {
        path: String,
        size: usize,
        limit: usize,
    },
}

pub type OperatorApiResult<T, E = OperatorApiError> = Result<T, E>;
