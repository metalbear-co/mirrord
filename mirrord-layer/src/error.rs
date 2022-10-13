use std::{env::VarError, ptr, str::ParseBoolError};

use errno::set_errno;
use kube::config::InferConfigError;
use libc::{c_char, FILE};
use mirrord_protocol::{tcp::LayerTcp, ConnectionId, ResponseError};
use thiserror::Error;
use tokio::sync::{mpsc::error::SendError, oneshot::error::RecvError};
use tracing::{error, info};

use super::HookMessage;

const IGNORE_ERROR_CODES: [i32; 2] = [libc::EINPROGRESS, libc::EAFNOSUPPORT];

fn should_ignore(code: Option<i32>) -> bool {
    if let Some(code) = code {
        IGNORE_ERROR_CODES.contains(&code)
    } else {
        false
    }
}

#[derive(Error, Debug)]
pub(crate) enum HookError {
    #[error("mirrord-layer: Failed while getting a response!")]
    ResponseError(#[from] ResponseError),

    #[error("mirrord-layer: DNS does not resolve!")]
    DNSNoName,

    #[error("mirrord-layer: Failed to `Lock` resource!")]
    LockError,

    #[error("mirrord-layer: Null pointer found!")]
    NullPointer,

    #[error("mirrord-layer: Receiver failed with `{0}`!")]
    RecvError(#[from] RecvError),

    #[error("mirrord-layer: IO failed with `{0}`!")]
    IO(#[from] std::io::Error),

    #[error("mirrord-layer: HOOK_SENDER is `None`!")]
    EmptyHookSender,

    #[error("mirrord-layer: Converting int failed with `{0}`!")]
    TryFromInt(#[from] std::num::TryFromIntError),

    #[error("mirrord-layer: Creating `CString` failed with `{0}`!")]
    Null(#[from] std::ffi::NulError),

    #[error("mirrord-layer: Failed converting `to_str` with `{0}`!")]
    Utf8(#[from] std::str::Utf8Error),

    #[error("mirrord-layer: Sender<HookMessage> failed with `{0}`!")]
    SendErrorHookMessage(#[from] SendError<HookMessage>),

    #[error("mirrord-layer: Failed creating local file for `remote_fd` `{0}`!")]
    LocalFileCreation(usize),
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

    #[error("mirrord-layer: Failed to get `Sender` for sending udp response!")]
    SendErrorUdpResponse,

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

    #[error("mirrord-layer: Failed to get `Spec` for Pod!")]
    PodSpecNotFound,

    #[error("mirrord-layer: Failed to get Pod for Job `{0}`!")]
    JobPodNotFound(String),

    #[error("mirrord-layer: Kube failed with error `{0}`!")]
    KubeError(#[from] kube::Error),

    #[error("mirrord-layer: JSON convert error")]
    JSONConvertError(#[from] serde_json::Error),

    #[error("mirrord-layer: Container not found: `{0}`")]
    ContainerNotFound(String),

    #[error("mirrord-layer: Node name wasn't found in pod spec")]
    NodeNotFound,

    #[error("mirrord-layer: Deployment: `{0} not found!`")]
    DeploymentNotFound(String),

    #[error("mirrord-layer: Invalid target proivded `{0:#?}`!")]
    InvalidTarget(String),

    #[error("mirrord-layer: Failed to get Container runtime data for `{0}`!")]
    ContainerRuntimeParseError(String),

    #[error("mirrord-layer: Pod name not found in response from kube API")]
    PodNameNotFound,

    #[error("mirrord-layer: Pod status not found in response from kube API")]
    PodStatusNotFound,

    #[error("mirrord-layer: Container status not found in response from kube API")]
    ContainerStatusNotFound,

    #[error("mirrord-layer: Container ID not found in response from kube API")]
    ContainerIdNotFound,
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
        match fail {
            HookError::ResponseError(ResponseError::NotFound(_))
            | HookError::ResponseError(ResponseError::NotFile(_))
            | HookError::ResponseError(ResponseError::NotDirectory(_))
            | HookError::ResponseError(ResponseError::Remote(_))
            | HookError::ResponseError(ResponseError::RemoteIO(_)) => {
                info!("libc error (doesn't indicate a problem) >> {:#?}", fail)
            }
            HookError::IO(ref e) if (should_ignore(e.raw_os_error())) => {
                info!("libc error (doesn't indicate a problem) >> {:#?}", fail)
            }
            _ => error!("Error occured in Layer >> {:?}", fail),
        };

        let libc_error = match fail {
            HookError::SendErrorHookMessage(_) => libc::EBADMSG,
            HookError::RecvError(_) => libc::EBADMSG,
            HookError::Null(_) => libc::EINVAL,
            HookError::TryFromInt(_) => libc::EINVAL,
            HookError::EmptyHookSender => libc::EINVAL,
            HookError::IO(io_fail) => io_fail.raw_os_error().unwrap_or(libc::EIO),
            HookError::LockError => libc::EINVAL,
            HookError::ResponseError(response_fail) => match response_fail {
                ResponseError::AllocationFailure(_) => libc::ENOMEM,
                ResponseError::NotFound(_) => libc::ENOENT,
                ResponseError::NotDirectory(_) => libc::ENOTDIR,
                ResponseError::NotFile(_) => libc::EISDIR,
                ResponseError::RemoteIO(io_fail) => io_fail.raw_os_error.unwrap_or(libc::EIO),
                ResponseError::Remote(remote) => match remote {
                    // So far only encountered when trying to make requests from golang.
                    mirrord_protocol::RemoteError::ConnectTimedOut(_) => libc::ENETUNREACH,
                    _ => libc::EINVAL,
                },
                ResponseError::DnsLookup(dns_fail) => match dns_fail.kind {
                    mirrord_protocol::ResolveErrorKindInternal::Timeout => libc::EAI_AGAIN,
                    _ => libc::EAI_FAIL,
                },
            },
            HookError::DNSNoName => libc::EFAULT,
            HookError::Utf8(_) => libc::EINVAL,
            HookError::NullPointer => libc::EINVAL,
            HookError::LocalFileCreation(_) => libc::EINVAL,
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

impl From<HookError> for *mut c_char {
    fn from(fail: HookError) -> Self {
        let _ = i64::from(fail);

        ptr::null_mut()
    }
}
