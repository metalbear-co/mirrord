use std::{env::VarError, os::unix::io::RawFd, str::ParseBoolError};

use mirrord_protocol::tcp::LayerTcp;
use thiserror::Error;
use tokio::sync::{mpsc::error::SendError, oneshot::error::RecvError};

use super::HookMessage;

#[derive(Error, Debug)]
pub enum LayerError {
    #[error("mirrord-layer: Environment variable interaction failed with `{0}`!")]
    VarError(#[from] VarError),

    #[error("mirrord-layer: Parsing `bool` value failed with `{0}`!")]
    ParseBoolError(#[from] ParseBoolError),

    #[error("mirrord-layer: Sender<HookMessage> failed with `{0}`!")]
    SendErrorHookMessage(#[from] SendError<HookMessage>),

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

//Todo: https://stackoverflow.com/questions/39150216/implementing-a-trait-for-multiple-types-at-once

// what should the mapping be between LayerError and integer types

// mapping based on - https://man7.org/linux/man-pages/man3/errno.3.html

impl From<LayerError> for i32 {
    fn from(error: LayerError) -> Self {
        match error {
            LayerError::VarError(_) => libc::EINVAL,
            LayerError::ParseBoolError(_) => 2,
            LayerError::SendErrorHookMessage(_) => libc::EBADMSG,
            LayerError::SendErrorConnection(_) => libc::ECOMM,
            LayerError::SendErrorLayerTcp(_) => libc::EBADMSG,
            LayerError::RecvError(_) => 6,
            LayerError::Null(_) => 7,
            LayerError::TryFromInt(_) => libc::EINVAL,
            LayerError::LocalFDNotFound(_) => libc::EBADF,
            LayerError::EmptyHookSender => libc::ENOENT,
            LayerError::NoConnectionId(_) => libc::ECONNREFUSED,
            LayerError::IO(_) => libc::EIO,
            LayerError::PortNotFound(_) => libc::EADDRNOTAVAIL,
            LayerError::ConnectionIdNotFound(_) => libc::EADDRNOTAVAIL,
            LayerError::ListenAlreadyExists => libc::EEXIST,
        }
    }
}
