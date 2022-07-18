use std::{env::VarError, os::unix::io::RawFd, ptr, str::ParseBoolError};

use errno::set_errno;
use kube::config::InferConfigError;
use libc::FILE;
use mirrord_protocol::{tcp::LayerTcp, ResponseError};
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

    #[error("mirrord-layer: Failed to get `Sender` for sending file response!")]
    SendErrorFileResponse,

    #[error("mirrord-layer: Failed to get `Sender` for sending tcp response!")]
    SendErrorTcpResponse,

    #[error("mirrord-layer: Failed to get `Sender` for sending getaddrinfo response!")]
    SendErrorGetAddrInfoResponse,

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

    #[error("mirrord-layer: Failed to `Lock` resource!")]
    LockError,

    #[error("mirrord-layer: Failed while getting a response!")]
    ResponseError(#[from] ResponseError),

    #[error("mirrord-layer: Unmatched pong!")]
    UnmatchedPong,

    #[error("mirrord-layer: Failed to get `KubeConfig`!")]
    KubeConfigError(#[from] InferConfigError),

    #[error("mirrord-layer: Failed to get `Spec` for Pod `{0}`!")]
    PodSpecNotFound(String),

    #[error("mirrord-layer: Kube failed with error `{0}`!")]
    KubeError(#[from] kube::Error),

    #[error("mirrord-layer: JSON convert error")]
    JSONConvertError(#[from] serde_json::Error),

    #[error("mirrord-layer: Timed Out!")]
    TimeOutError,

    #[error("mirrord-layer: DNS does not resolve!")]
    DNSNoName,

    #[error("mirrord-layer: Failed converting `to_str` with `{0}`!")]
    Utf8(#[from] std::str::Utf8Error),

    #[error("mirrord-layer: Failed converting `sockaddr`!")]
    AddressConversion,

    #[error("mirrord-layer: Failed request to create socket with domain `{0}`!")]
    UnsupportedDomain(i32),
}

// Cannot have a generic From<T> implementation for this error, so explicitly implemented here.
impl<'a, T> From<std::sync::PoisonError<std::sync::MutexGuard<'a, T>>> for LayerError {
    fn from(_: std::sync::PoisonError<std::sync::MutexGuard<T>>) -> Self {
        LayerError::LockError
    }
}

// mapping based on - https://man7.org/linux/man-pages/man3/errno.3.html

impl From<LayerError> for i64 {
    fn from(fail: LayerError) -> Self {
        error!("Error occured in Layer >> {:?}", fail);

        let libc_error = match fail {
            LayerError::VarError(_) => libc::EINVAL,
            LayerError::ParseBoolError(_) => libc::EINVAL,
            LayerError::SendErrorHookMessage(_) => libc::EBADMSG,
            LayerError::SendErrorConnection(_) => libc::EBADMSG,
            LayerError::SendErrorLayerTcp(_) => libc::EBADMSG,
            LayerError::RecvError(_) => libc::EBADMSG,
            LayerError::Null(_) => libc::EINVAL,
            LayerError::TryFromInt(_) => libc::EINVAL,
            LayerError::LocalFDNotFound(..) => libc::EBADF,
            LayerError::EmptyHookSender => libc::EINVAL,
            LayerError::NoConnectionId(_) => libc::ECONNREFUSED,
            LayerError::IO(io_fail) => io_fail.raw_os_error().unwrap_or(libc::EIO),
            LayerError::PortNotFound(_) => libc::EADDRNOTAVAIL,
            LayerError::ConnectionIdNotFound(_) => libc::EADDRNOTAVAIL,
            LayerError::ListenAlreadyExists => libc::EEXIST,
            LayerError::SendErrorFileResponse => libc::EINVAL,
            LayerError::SendErrorTcpResponse => libc::EINVAL,
            LayerError::SendErrorGetAddrInfoResponse => libc::EINVAL,
            LayerError::LockError => libc::EINVAL,
            LayerError::ResponseError(response_fail) => match response_fail {
                ResponseError::AllocationFailure(_) => libc::ENOMEM,
                ResponseError::NotFound(_) => libc::ENOENT,
                ResponseError::NotDirectory(_) => libc::ENOTDIR,
                ResponseError::NotFile(_) => libc::EISDIR,
                ResponseError::RemoteIO(io_fail) => io_fail.raw_os_error.unwrap_or(libc::EIO),
            },
            LayerError::UnmatchedPong => libc::ETIMEDOUT,
            LayerError::KubeConfigError(_) => libc::EINVAL,
            LayerError::PodSpecNotFound(_) => libc::EINVAL,
            LayerError::KubeError(_) => libc::EINVAL,
            LayerError::JSONConvertError(_) => libc::EINVAL,
            LayerError::TimeOutError => libc::ETIMEDOUT,
            LayerError::DNSNoName => libc::EFAULT,
            LayerError::Utf8(_) => libc::EINVAL,
            LayerError::AddressConversion => libc::EINVAL,
            LayerError::UnsupportedDomain(_) => libc::EINVAL,
        };

        set_errno(errno::Errno(libc_error));

        -1
    }
}

impl From<LayerError> for isize {
    fn from(fail: LayerError) -> Self {
        i64::from(fail) as _
    }
}

impl From<LayerError> for usize {
    fn from(fail: LayerError) -> Self {
        let _ = i64::from(fail);

        0
    }
}

impl From<LayerError> for i32 {
    fn from(fail: LayerError) -> Self {
        i64::from(fail) as _
    }
}

impl From<LayerError> for *mut FILE {
    fn from(fail: LayerError) -> Self {
        let _ = i64::from(fail);

        ptr::null_mut()
    }
}
