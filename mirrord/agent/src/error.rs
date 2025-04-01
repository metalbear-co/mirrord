use std::{process::ExitStatus, sync::Arc};

use thiserror::Error;

use crate::{
    client_connection::TlsSetupError, incoming::RedirectorTaskError, namespace::NamespaceError,
    runtime, util::remote_runtime::RemoteRuntimeError,
};

#[derive(Debug, Error)]
pub(crate) enum AgentError {
    #[error("io error: {0}")]
    IO(#[from] std::io::Error),

    #[error("Container runtime error: {0}")]
    ContainerRuntimeError(#[from] runtime::ContainerRuntimeError),

    #[error("Path failed with `{0}`")]
    StripPrefixError(#[from] std::path::StripPrefixError),

    #[error(r#"Failed to set socket flag PACKET_IGNORE_OUTGOING, this might be due to kernel version before 4.20.
    Original error `{0}`"#)]
    PacketIgnoreOutgoing(#[source] std::io::Error),

    #[error("Background task `{task}` failed: `{error}`")]
    BackgroundTaskFailed {
        task: &'static str,
        #[source]
        error: Arc<dyn std::error::Error + Send + Sync>,
    },

    #[error("Returning an error to test the agent's error cleanup. Should only ever be used when testing mirrord.")]
    TestError,

    #[error(transparent)]
    FailedNamespaceEnter(#[from] NamespaceError),

    #[error("TLS setup failed: {0}")]
    TlsSetupError(#[from] TlsSetupError),

    /// Child agent process spawned in `main` failed.
    #[error("Agent child process failed: {0}")]
    AgentFailed(ExitStatus),

    #[error("Exhausted possible identifiers for incoming connections.")]
    ExhaustedConnectionId,

    #[error("Timeout on accepting first client connection")]
    FirstConnectionTimeout,

    #[error("Incoming traffic redirector failed: {0}")]
    PortRedirectorError(#[from] RedirectorTaskError),

    #[error("IP tables setup failed: {0}")]
    IPTablesSetupError(#[source] Box<dyn std::error::Error + Send + Sync + 'static>),

    #[error("Failed to start a tokio runtime in the target's namespace: {0}")]
    RemoteRuntimeError(#[from] RemoteRuntimeError),
}

pub(crate) type AgentResult<T, E = AgentError> = std::result::Result<T, E>;
