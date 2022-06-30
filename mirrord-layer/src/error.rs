use std::{env::VarError, os::unix::io::RawFd, str::ParseBoolError, path::PathBuf};

use errno::set_errno;
use mirrord_protocol::tcp::LayerTcp;
use thiserror::Error;
use tokio::sync::{mpsc::error::SendError, oneshot::error::RecvError};
use tracing::error;

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
    LocalFDNotFound(RawFd, PathBuf),

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

// mapping based on - https://man7.org/linux/man-pages/man3/errno.3.html

impl From<LayerError> for i32 {
    fn from(error: LayerError) -> Self {
        error!("Error occured in Layer >> {:?}", error);
        let libc_error = match error {
            LayerError::VarError(_) => libc::EINVAL,
            LayerError::ParseBoolError(_) => libc::EINVAL,
            LayerError::SendErrorHookMessage(_) => libc::EBADMSG,
            LayerError::SendErrorConnection(_) => libc::EBADMSG,
            LayerError::SendErrorLayerTcp(_) => libc::EBADMSG,
            LayerError::RecvError(_) => libc::EBADMSG,
            LayerError::Null(_) => libc::EINVAL,
            LayerError::TryFromInt(_) => libc::EINVAL,
            LayerError::LocalFDNotFound(..) => libc::EBADF,
            LayerError::EmptyHookSender => libc::ENOENT,
            LayerError::NoConnectionId(_) => libc::ECONNREFUSED,
            LayerError::IO(io_err) => io_err.raw_os_error().unwrap_or(libc::EIO),
            LayerError::PortNotFound(_) => libc::EADDRNOTAVAIL,
            LayerError::ConnectionIdNotFound(_) => libc::EADDRNOTAVAIL,
            LayerError::ListenAlreadyExists => libc::EEXIST,            
        };
        set_errno(errno::Errno(libc_error));
        libc_error
    }
}
