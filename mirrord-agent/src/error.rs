use mirrord_protocol::{DaemonMessage, FileRequest, FileResponse};
use thiserror::Error;

use crate::sniffer::{SnifferCommand, SnifferOutput};

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("Agent failed with `{0}`")]
    IO(#[from] std::io::Error),

    #[error("SnifferCommand sender failed with `{0}`")]
    SendSnifferCommand(#[from] tokio::sync::mpsc::error::SendError<SnifferCommand>),

    #[error("SnifferOutput sender failed with `{0}`")]
    SendSnifferOutput(#[from] tokio::sync::mpsc::error::SendError<SnifferOutput>),

    #[error("FileRequest sender failed with `{0}`")]
    SendFileRequest(#[from] tokio::sync::mpsc::error::SendError<(u32, FileRequest)>),

    #[error("FileResponse sender failed with `{0}`")]
    SendFileResponse(#[from] tokio::sync::mpsc::error::SendError<(u32, FileResponse)>),

    #[error("DaemonMessage sender failed with `{0}`")]
    SendDaemonMessage(#[from] tokio::sync::mpsc::error::SendError<DaemonMessage>),

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

    #[error("Pcap failed with `{0}`")]
    Pcap(#[from] pcap::Error),

    #[error("Path failed with `{0}`")]
    StripPrefixError(#[from] std::path::StripPrefixError),

    #[error("Bollard failed with `{0}`")]
    Bollard(#[from] bollard::errors::Error),
}
