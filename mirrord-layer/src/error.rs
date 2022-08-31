use std::{env::VarError, os::unix::io::RawFd, ptr, str::ParseBoolError};

use errno::set_errno;
use kube::config::InferConfigError;
use libc::FILE;
use mirrord_protocol::{tcp::LayerTcp, ConnectionId, ResponseError};
use thiserror::Error;
use tokio::sync::{mpsc::error::SendError, oneshot::error::RecvError};
use tracing::{error, warn};

use super::HookMessage;

#[derive(Error, Debug)]
pub(crate) enum HookError {
    #[error("mirrord-layer: Failed while getting a response!")]
    ResponseError(#[from] ResponseError),

    #[error("mirrord-layer: DNS does not resolve!")]
    DNSNoName,

    #[error("mirrord-layer: Failed to `Lock` resource!")]
    LockError,

    #[error("mirrord-layer: Socket operation called on port `{0}` that is not handled by us!")]
    BypassedPort(u16),

    #[error("mirrord-layer: Socket operation called with type `{0}` that is not handled by us!")]
    BypassedType(i32),

    #[error("mirrord-layer: Socket operation called with domain `{0}` that is not handled by us!")]
    BypassedDomain(i32),

    #[error("mirrord-layer: Socket `{0}` is in an invalid state!")]
    SocketInvalidState(RawFd),

    #[error("mirrord-layer: Null pointer found!")]
    NullPointer,

    #[error("mirrord-layer: Receiver failed with `{0}`!")]
    RecvError(#[from] RecvError),

    #[error("mirrord-layer: IO failed with `{0}`!")]
    IO(#[from] std::io::Error),

    #[error("mirrord-layer: Failed to find local fd `{0}`!")]
    LocalFDNotFound(RawFd),

    #[error("mirrord-layer: HOOK_SENDER is `None`!")]
    EmptyHookSender,

    #[error("mirrord-layer: Failed converting `sockaddr`!")]
    AddressConversion,

    #[error("mirrord-layer: Converting int failed with `{0}`!")]
    TryFromInt(#[from] std::num::TryFromIntError),

    #[error("mirrord-layer: Failed request to create socket with domain `{0}`!")]
    UnsupportedDomain(i32),

    #[error("mirrord-layer: Creating `CString` failed with `{0}`!")]
    Null(#[from] std::ffi::NulError),

    #[error("mirrord-layer: Failed converting `to_str` with `{0}`!")]
    Utf8(#[from] std::str::Utf8Error),

    #[error("mirrord-layer: Sender<HookMessage> failed with `{0}`!")]
    SendErrorHookMessage(#[from] SendError<HookMessage>),
}

#[derive(Error, Debug)]
pub(crate) enum LayerError {
    #[error("mirrord-layer: Failed while getting a response!")]
    ResponseError(#[from] ResponseError),

    #[error("mirrord-layer: Frida failed with `{0}`!")]
    Frida(#[from] frida_gum::Error),

    #[error("mirrord-layer: Failed to find export for name `{0}`!")]
    NoExportName(String),

    #[error("mirrord-layer: Environment variable interaction failed with `{0}`!")]
    VarError(#[from] VarError),

    #[error("mirrord-layer: Parsing `bool` value failed with `{0}`!")]
    ParseBoolError(#[from] ParseBoolError),

    #[error("mirrord-layer: Sender<Vec<u8>> failed with `{0}`!")]
    SendErrorConnection(#[from] SendError<Vec<u8>>),

    #[error("mirrord-layer: Sender<LayerTcp> failed with `{0}`!")]
    SendErrorLayerTcp(#[from] SendError<LayerTcp>),

    #[error("mirrord-layer: Failed to get `Sender` for sending tcp response!")]
    SendErrorTcpResponse,

    #[error("mirrord-layer: JoinError failed with `{0}`!")]
    Join(#[from] tokio::task::JoinError),

    #[error("mirrord-layer: Failed to get `Sender` for sending file response!")]
    SendErrorFileResponse,

    #[error("mirrord-layer: Failed to get `Sender` for sending getaddrinfo response!")]
    SendErrorGetAddrInfoResponse,

    #[error("mirrord-layer: IO failed with `{0}`!")]
    IO(#[from] std::io::Error),

    #[error("mirrord-layer: No connection found for id `{0}`!")]
    NoConnectionId(ConnectionId),

    #[error("mirrord-layer: Failed to find port `{0}`!")]
    PortNotFound(u16),

    #[error("mirrord-layer: Failed inserting listen, already exists!")]
    ListenAlreadyExists,

    #[error("mirrord-layer: Unmatched pong!")]
    UnmatchedPong,

    #[error("mirrord-layer: Failed to get `KubeConfig`!")]
    KubeConfigError(#[from] InferConfigError),

    #[error("mirrord-layer: Failed to get `Spec` for Pod `{0}`!")]
    PodSpecNotFound(String),

    #[error("mirrord-layer: Failed to get Pod for Job `{0}`!")]
    PodNotFound(String),

    #[error("mirrord-layer: Kube failed with error `{0}`!")]
    KubeError(#[from] kube::Error),

    #[error("mirrord-layer: JSON convert error")]
    JSONConvertError(#[from] serde_json::Error),

    #[error("mirrord-layer: Container `{0}` not found in namespace `{1}` pod `{2}`")]
    ContainerNotFound(String, String, String),
}

// Cannot have a generic From<T> implementation for this error, so explicitly implemented here.
impl<'a, T> From<std::sync::PoisonError<std::sync::MutexGuard<'a, T>>> for HookError {
    fn from(_: std::sync::PoisonError<std::sync::MutexGuard<T>>) -> Self {
        HookError::LockError
    }
}

pub(crate) type Result<T> = std::result::Result<T, LayerError>;
pub(crate) type HookResult<T> = std::result::Result<T, HookError>;

/// mapping based on - https://man7.org/linux/man-pages/man3/errno.3.html
impl From<HookError> for i64 {
    fn from(fail: HookError) -> Self {
        // TODO: These recoverable errors should probably be a "sub-Error" from `HookError`, so that
        // we can do a single `match` for them everywhere, and avoid forgetting 1.
        // To get a better sense of what I mean by this, imagine if we forget to check
        // `BypassedDomain` in `socket::hooks::socket_detour` (we would error out, instead of
        // bypassing as expected).
        match fail {
            HookError::SocketInvalidState(_)
            | HookError::LocalFDNotFound(_)
            | HookError::BypassedType(_)
            | HookError::BypassedDomain(_)
            | HookError::BypassedPort(_) => {
                warn!("Recoverable issue >> {:#?}", fail)
            }
            _ => error!("Error occured in Layer >> {:?}", fail),
        };

        let libc_error = match fail {
            HookError::SendErrorHookMessage(_) => libc::EBADMSG,
            HookError::RecvError(_) => libc::EBADMSG,
            HookError::Null(_) => libc::EINVAL,
            HookError::TryFromInt(_) => libc::EINVAL,
            HookError::LocalFDNotFound(..) => libc::EBADF,
            HookError::EmptyHookSender => libc::EINVAL,
            HookError::IO(io_fail) => io_fail.raw_os_error().unwrap_or(libc::EIO),
            HookError::LockError => libc::EINVAL,
            HookError::ResponseError(response_fail) => match response_fail {
                ResponseError::AllocationFailure(_) => libc::ENOMEM,
                ResponseError::NotFound(_) => libc::ENOENT,
                ResponseError::NotDirectory(_) => libc::ENOTDIR,
                ResponseError::NotFile(_) => libc::EISDIR,
                ResponseError::RemoteIO(io_fail) => io_fail.raw_os_error.unwrap_or(libc::EIO),
            },
            HookError::DNSNoName => libc::EFAULT,
            HookError::Utf8(_) => libc::EINVAL,
            HookError::AddressConversion => libc::EINVAL,
            HookError::UnsupportedDomain(_) => libc::EINVAL,
            HookError::BypassedPort(_) => libc::EINVAL,
            HookError::BypassedType(_) => libc::EINVAL,
            HookError::BypassedDomain(_) => libc::EINVAL,
            HookError::SocketInvalidState(_) => libc::EINVAL,
            HookError::NullPointer => libc::EINVAL,
        };

        set_errno(errno::Errno(libc_error));

        -1
    }
}

impl From<HookError> for isize {
    fn from(fail: HookError) -> Self {
        i64::from(fail) as _
    }
}

impl From<HookError> for usize {
    fn from(fail: HookError) -> Self {
        let _ = i64::from(fail);

        0
    }
}

impl From<HookError> for i32 {
    fn from(fail: HookError) -> Self {
        i64::from(fail) as _
    }
}

impl From<HookError> for *mut FILE {
    fn from(fail: HookError) -> Self {
        let _ = i64::from(fail);

        ptr::null_mut()
    }
}
