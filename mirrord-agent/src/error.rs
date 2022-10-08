use mirrord_protocol::{
    outgoing::{
        tcp::{DaemonTcpOutgoing, LayerTcpOutgoing},
        udp::{DaemonUdpOutgoing, LayerUdpOutgoing},
        LayerConnect,
    },
    tcp::{DaemonTcp, LayerTcpSteal},
    FileRequest, FileResponse, Port,
};
use thiserror::Error;

use crate::sniffer::SnifferCommand;

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("Agent failed with `{0}`")]
    IO(#[from] std::io::Error),

    #[error("SnifferCommand sender failed with `{0}`")]
    SendSnifferCommand(#[from] tokio::sync::mpsc::error::SendError<SnifferCommand>),

    #[error("FileRequest sender failed with `{0}`")]
    SendFileRequest(#[from] tokio::sync::mpsc::error::SendError<(u32, FileRequest)>),

    #[error("FileResponse sender failed with `{0}`")]
    SendFileResponse(#[from] tokio::sync::mpsc::error::SendError<(u32, FileResponse)>),

    #[error("DaemonTcp sender failed with `{0}`")]
    SendDaemonTcp(#[from] tokio::sync::mpsc::error::SendError<DaemonTcp>),

    #[error("ConnectRequest sender failed with `{0}`")]
    SendConnectRequest(#[from] tokio::sync::mpsc::error::SendError<LayerConnect>),

    #[error("OutgoingTrafficRequest sender failed with `{0}`")]
    SendOutgoingTrafficRequest(#[from] tokio::sync::mpsc::error::SendError<LayerTcpOutgoing>),

    #[error("UdpOutgoingTrafficRequest sender failed with `{0}`")]
    SendUdpOutgoingTrafficRequest(#[from] tokio::sync::mpsc::error::SendError<LayerUdpOutgoing>),

    #[error("Receiver channel is closed!")]
    ReceiverClosed,

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
}

pub type Result<T> = std::result::Result<T, AgentError>;
