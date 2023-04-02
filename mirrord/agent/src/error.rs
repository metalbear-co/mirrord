use mirrord_protocol::{
    outgoing::{
        tcp::{DaemonTcpOutgoing, LayerTcpOutgoing},
        udp::{DaemonUdpOutgoing, LayerUdpOutgoing},
        LayerConnect,
    },
    tcp::{DaemonTcp, LayerTcpSteal},
    DaemonMessage, FileRequest, FileResponse, Port,
};
use thiserror::Error;

use crate::{sniffer::SnifferCommand, steal::StealerCommand};

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("Agent failed with `{0:?}`")]
    IO(#[from] std::io::Error),

    #[error("SnifferCommand sender failed with `{0}`")]
    SendSnifferCommand(#[from] tokio::sync::mpsc::error::SendError<SnifferCommand>),

    #[error("StealerCommand sender failed with `{0}`")]
    SendStealerCommand(#[from] tokio::sync::mpsc::error::SendError<StealerCommand>),

    #[error("FileRequest sender failed with `{0}`")]
    SendFileRequest(#[from] tokio::sync::mpsc::error::SendError<(u32, FileRequest)>),

    #[error("FileResponse sender failed with `{0}`")]
    SendFileResponse(#[from] tokio::sync::mpsc::error::SendError<(u32, FileResponse)>),

    #[error("DaemonTcp sender failed with `{0}`")]
    SendDaemonTcp(#[from] tokio::sync::mpsc::error::SendError<DaemonTcp>),

    #[error("DaemonMessage sender failed with `{0}`")]
    SendDaemonMessage(#[from] tokio::sync::broadcast::error::SendError<DaemonMessage>),

    #[error("StealerCommand sender failed with `{0}`")]
    TrySendStealerCommand(#[from] tokio::sync::mpsc::error::TrySendError<StealerCommand>),

    #[error("ConnectRequest sender failed with `{0}`")]
    SendConnectRequest(#[from] tokio::sync::mpsc::error::SendError<LayerConnect>),

    #[error("OutgoingTrafficRequest sender failed with `{0}`")]
    SendOutgoingTrafficRequest(#[from] tokio::sync::mpsc::error::SendError<LayerTcpOutgoing>),

    #[error("UdpOutgoingTrafficRequest sender failed with `{0}`")]
    SendUdpOutgoingTrafficRequest(#[from] tokio::sync::mpsc::error::SendError<LayerUdpOutgoing>),

    #[error("Receiver channel is closed!")]
    ReceiverClosed,

    #[error("Request channel closed unexpectedly.")]
    HttpRequestReceiverClosed,

    #[error("ConnectRequest sender failed with `{0}`")]
    SendOutgoingTrafficResponse(#[from] tokio::sync::mpsc::error::SendError<DaemonTcpOutgoing>),

    #[error("UdpConnectRequest sender failed with `{0}`")]
    SendUdpOutgoingTrafficResponse(#[from] tokio::sync::mpsc::error::SendError<DaemonUdpOutgoing>),

    #[error("task::Join failed with `{0}`")]
    Join(#[from] tokio::task::JoinError),

    #[error("time::Elapsed failed with `{0}`")]
    Elapsed(#[from] tokio::time::error::Elapsed),

    #[error("tonic::Transport failed with `{0}`")]
    Transport(#[from] containerd_client::tonic::transport::Error),

    #[error("tonic::Status failed with `{0}`")]
    Status(#[from] containerd_client::tonic::Status),

    #[error("NotFound failed with `{0}`")]
    NotFound(String),

    #[error("Serde failed with `{0}`")]
    SerdeJson(#[from] serde_json::Error),

    #[error("Errno failed with `{0}`")]
    Errno(#[from] nix::errno::Errno),

    #[error("Path failed with `{0}`")]
    StripPrefixError(#[from] std::path::StripPrefixError),

    #[error("Bollard failed with `{0}`")]
    Bollard(#[from] bollard::errors::Error),

    #[error("Connection received from unexepcted port `{0}`")]
    UnexpectedConnection(Port),

    #[error("LayerTcpSteal sender failed with `{0}`")]
    SendLayerTcpSteal(#[from] tokio::sync::mpsc::error::SendError<LayerTcpSteal>),

    #[error("IPTables failed with `{0}`")]
    IPTablesError(String),

    #[error("Join task failed")]
    JoinTask,

    #[error("DNS request send failed with `{0}`")]
    DnsRequestSendError(#[from] tokio::sync::mpsc::error::SendError<crate::dns::DnsRequest>),

    #[error("DNS response receive failed with `{0}`")]
    DnsResponseReceiveError(#[from] tokio::sync::oneshot::error::RecvError),

    #[error("Pause was set, but container id or runtime is missing.")]
    MissingContainerInfo,

    #[error("start_client -> Ran out of connections, dropping new connection")]
    ConnectionLimitReached,

    #[error("An internal invariant of the agent was violated, this should not happen.")]
    AgentInvariantViolated,

    #[error(r#"Failed to set socket flag PACKET_IGNORE_OUTGOING, this might be due to kernel version before 4.20.
    Original error `{0}`"#)]
    PacketIgnoreOutgoing(#[source] std::io::Error),

    #[error("Reading request body failed with `{0}`")]
    HttpRequestSerializationError(#[from] hyper::Error),

    #[error("HTTP filter-stealing error: `{0}`")]
    HttpFilterError(#[from] crate::steal::http::error::HttpTrafficError),

    #[error("Failed to encode a an HTTP response with error: `{0}`")]
    HttpEncoding(#[from] hyper::http::Error),

    #[error(
        r#"Couldn't send message to sniffer (mirror) api, sniffer probably not running. 
        Possible reason can be node kernel version before 4.20
        Check agent logs for errors and please report a bug if kernel version >=4.20"#
    )]
    SnifferApiError,

    #[error(
        r#"Couldn't find containerd socket to use, please open a bug report
           providing information on how you installed k8s and if you know where
           the containerd socket is"#
    )]
    ContainerdSocketNotFound,

    #[error("Returning an error to test the agent's error cleanup. Should only ever be used when testing mirrord.")]
    TestError,
}

pub(crate) type Result<T, E = AgentError> = std::result::Result<T, E>;
