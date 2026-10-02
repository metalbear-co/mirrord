use std::{process::ExitStatus, sync::Arc};

use mirrord_cluster_auth::ClusterAuthError;
use mirrord_nightly_polyfill::error::Report;
use mirrord_sessions_manager_client::SessionsManagerClientError;
use thiserror::Error;

use crate::{
    client_connection::TlsSetupError, http::filter::FilterCreationError,
    incoming::RedirectorTaskError, namespace::NamespaceError, runtime,
    util::error::AgentRuntimeError, workload_companion::RemoteIncomingHandoffError,
};

#[derive(Debug, Error)]
pub(crate) enum AgentError {
    #[error("io error: {0}")]
    IO(#[from] std::io::Error),

    #[error("Container runtime error: {0}")]
    ContainerRuntimeError(#[from] runtime::ContainerRuntimeError),

    #[error("Path failed with `{0}`")]
    StripPrefixError(#[from] std::path::StripPrefixError),

    #[error("Background task `{task}` failed: `{error}`")]
    BackgroundTaskFailed {
        task: &'static str,
        #[source]
        error: Arc<dyn std::error::Error + Send + Sync>,
    },

    #[error(
        "Returning an error to test the agent's error cleanup. Should only ever be used when testing mirrord."
    )]
    TestError,

    #[error(transparent)]
    FailedNamespaceEnter(#[from] NamespaceError),

    #[error("TLS setup failed: {0}")]
    TlsSetupError(#[from] TlsSetupError),

    /// Child agent process spawned in `main` failed.
    #[error("Agent child process failed: {0}")]
    AgentFailed(ExitStatus),

    #[error("Exhausted possible identifiers for tunneled connections.")]
    ExhaustedConnectionId,

    #[error("Failed to parse the given HTTP filter: {0}")]
    InvalidHttpFilter(
        /// Boxed due to large size difference.
        #[from]
        Box<FilterCreationError>,
    ),

    #[error("Timeout on accepting first client connection")]
    FirstConnectionTimeout,

    #[error("Incoming traffic redirector failed: {0}")]
    PortRedirectorError(#[from] RedirectorTaskError),

    #[error("IP tables setup failed: {0}")]
    IPTablesSetupError(#[source] Box<dyn std::error::Error + Send + Sync + 'static>),

    #[error("IP tables dirty")]
    IPTablesDirty,

    #[error("Failed to start a tokio runtime in the target's namespace: {0}")]
    RemoteRuntimeError(#[from] AgentRuntimeError),

    #[error(transparent)]
    Timeout(#[from] tokio::time::error::Elapsed),

    #[error("Failed to create connection from sessions-manager: {0}")]
    SessionsManagerClientError(#[from] SessionsManagerClientError),

    #[error("Connection handoff failed: {0}")]
    RemoteIncomingHandoffError(#[from] RemoteIncomingHandoffError),

    #[error("{0} is required when MIRRORD_OPERATOR_API_URL is set")]
    MissingOperatorConfig(&'static str),

    #[error(
        "MIRRORD_SESSIONS_MANAGER_URL and MIRRORD_OPERATOR_API_URL are mutually exclusive; set \
         the first for a standalone sessions-manager, the second for one hosted by the mirrord \
         operator"
    )]
    ConflictingSessionsManagerEndpoints,

    #[error(
        "no AWS region to sign the EKS token for: set AWS_REGION, or use the cluster's EKS \
         endpoint in MIRRORD_OPERATOR_API_URL"
    )]
    MissingOperatorRegion,

    #[error("Failed to connect to the operator's cluster: {}", Report::new(.0))]
    OperatorClusterConnection(#[from] ClusterAuthError),
}

pub(crate) type AgentResult<T, E = AgentError> = std::result::Result<T, E>;
