use std::{env::VarError, os::unix::io::RawFd, str::ParseBoolError};

use mirrord_protocol::tcp::LayerTcp;
use thiserror::Error;
use tokio::sync::{mpsc::error::SendError, oneshot::error::RecvError};

use super::{tcp::TcpHandlerMessage, HookMessage};

#[derive(Error, Debug)]
pub enum LayerError {
    #[error("mirrord-layer: Environment variable interaction failed with `{0}`!")]
    VarError(#[from] VarError),

    #[error("mirrord-layer: Parsing `bool` value failed with `{0}`!")]
    ParseBoolError(#[from] ParseBoolError),

    #[error("mirrord-layer: Sender<HookMessage> failed with `{0}`!")]
    SendErrorHookMessage(#[from] SendError<HookMessage>),

    #[error("mirrord-layer: Sender<TcpHandlerMessage> failed with `{0}`!")]
    SendErrorTcpHandlerMessage(#[from] SendError<TcpHandlerMessage>),

    #[error("mirrord-layer: Sender<Vec<u8>> failed with `{0}`!")]
    SendErrorConnection(#[from] SendError<Vec<u8>>),

    #[error("mirrord-layer: Sender<LayerTcp> failed with `{0}`!")]
    SendErrorLayerTcp(#[from] SendError<LayerTcp>),

    #[error("mirrord-layer: Receiver failed with `{0}`!")]
    RecvError(#[from] RecvError),

    #[error("mirrord-layer: Creating `CString` failed with `{0}`!")]
    Null(#[from] std::ffi::NulError),

    #[error("mirrord-layer: Converting int failed with `{0}`!")]
    TryFromInt(#[from] std::num::TryFromIntError),

    #[error("mirrord-layer: Failed to find local fd `{0}`!")]
    LocalFDNotFound(RawFd),

    #[error("mirrord-layer: HOOK_SENDER is `None`!")]
    EmptyHookSender,

    #[error("mirrord-layer: No connection found for id `{0}`!")]
    NoConnectionId(u16),

    #[error("mirrord-layer: IO failed with `{0}`!")]
    IO(#[from] std::io::Error),

    #[error("mirrord-layer: Failed to find port `{0}`!")]
    PortNotFound(u16),

    #[error("mirrord-layer: Failed to find connection_id `{0}`!")]
    ConnectionIdNotFound(u16),

    #[error("mirrord-layer: Failed inserting listen, already exists!")]
    ListenAlreadyExists,
}
