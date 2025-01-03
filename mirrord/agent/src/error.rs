use std::process::ExitStatus;

use mirrord_protocol::outgoing::udp::DaemonUdpOutgoing;
use thiserror::Error;
use tokio::sync::mpsc::{self, error::SendError};

use crate::{
    client_connection::TlsSetupError, namespace::NamespaceError, runtime,
    sniffer::messages::SnifferCommand, steal::StealerCommand,
};

#[derive(Debug, Error)]
pub(crate) enum AgentError {
    #[error("io error: {0}")]
    IO(#[from] std::io::Error),

    #[error("SnifferCommand sender failed with `{0}`")]
    SendSnifferCommand(#[from] SendError<SnifferCommand>),

    #[error("TCP stealer task is dead")]
    TcpStealerTaskDead,

    #[error("UdpConnectRequest sender failed with `{0}`")]
    SendUdpOutgoingTrafficResponse(#[from] SendError<DaemonUdpOutgoing>),

    #[error("task::Join failed with `{0}`")]
    Join(#[from] tokio::task::JoinError),

    #[error("Container runtime error: {0}")]
    ContainerRuntimeError(#[from] runtime::ContainerRuntimeError),

    #[error("Path failed with `{0}`")]
    StripPrefixError(#[from] std::path::StripPrefixError),

    #[error("IPTables failed with `{0}`")]
    IPTablesError(String),

    #[error("Join task failed")]
    JoinTask,

    #[error("DNS request send failed with `{0}`")]
    DnsRequestSendError(#[from] SendError<crate::dns::DnsCommand>),

    #[error("DNS response receive failed with `{0}`")]
    DnsResponseReceiveError(#[from] tokio::sync::oneshot::error::RecvError),

    #[error(r#"Failed to set socket flag PACKET_IGNORE_OUTGOING, this might be due to kernel version before 4.20.
    Original error `{0}`"#)]
    PacketIgnoreOutgoing(#[source] std::io::Error),

    #[error(
        r#"Couldn't send message to sniffer (mirror) api, sniffer probably not running. 
        Possible reason can be node kernel version before 4.20
        Check agent logs for errors and please report a bug if kernel version >=4.20"#
    )]
    SnifferNotRunning,

    #[error("Couldn't send message to stealer (steal) api, stealer probably not running.")]
    StealerNotRunning,

    #[error("Background task `{task}` failed with `{cause}`")]
    BackgroundTaskFailed { task: &'static str, cause: String },

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

    #[allow(dead_code)]
    /// Temporary error for vpn feature
    #[error("Generic error in vpn: {0}")]
    VpnError(String),

    /// When we neither create a redirector for IPv4, nor for IPv6
    #[error("Could not create a listener for stolen connections")]
    CannotListenForStolenConnections,
}

impl From<mpsc::error::SendError<StealerCommand>> for AgentError {
    fn from(_: mpsc::error::SendError<StealerCommand>) -> Self {
        Self::TcpStealerTaskDead
    }
}

pub(crate) type Result<T, E = AgentError> = std::result::Result<T, E>;
