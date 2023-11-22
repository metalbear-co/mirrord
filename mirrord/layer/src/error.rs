use std::{env::VarError, net::SocketAddr, ptr, str::ParseBoolError};

use errno::set_errno;
use ignore_codes::*;
use libc::{c_char, hostent, DIR, FILE};
use mirrord_config::{config::ConfigError, feature::network::outgoing::OutgoingFilterError};
use mirrord_protocol::{ResponseError, SerializationError};
#[cfg(target_os = "macos")]
use mirrord_sip::SipError;
use thiserror::Error;
use tracing::{error, info};

use crate::proxy_connection::ProxyError;

/// Private module for preventing access to the [`IGNORE_ERROR_CODES`] constant.
mod ignore_codes {
    /// Error codes from [`libc`] that are **not** hard errors, meaning the operation may progress.
    ///
    /// Prefer using [`is_ignored_code`] instead of relying on this constant.
    const IGNORE_ERROR_CODES: [i32; 3] = [libc::EINPROGRESS, libc::EAFNOSUPPORT, libc::EADDRINUSE];

    /// Checks if an error code from some [`libc`] function should be treated as a hard error, or
    /// not.
    pub(super) fn is_ignored_code(code: Option<i32>) -> bool {
        if let Some(code) = code {
            IGNORE_ERROR_CODES.contains(&code)
        } else {
            false
        }
    }
}

/// Errors that occur in the layer's hook functions, and will reach the user's application.
///
/// These errors are converted to [`libc`] error codes, and are also used to [`set_errno`].
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

    #[error("mirrord-layer: IO failed with `{0}`!")]
    IO(#[from] std::io::Error),

    #[error("mirrord-layer: Could not get PROXY_CONNECTION, can't send a hook message!")]
    CannotGetProxyConnection,

    #[error("mirrord-layer: Converting int failed with `{0}`!")]
    TryFromInt(#[from] std::num::TryFromIntError),

    #[error("mirrord-layer: Creating `CString` failed with `{0}`!")]
    Null(#[from] std::ffi::NulError),

    #[error("mirrord-layer: Failed converting `to_str` with `{0}`!")]
    Utf8(#[from] std::str::Utf8Error),

    #[error("mirrord-layer: Failed creating local file for `remote_fd` `{0}`!")]
    LocalFileCreation(u64),

    #[cfg(target_os = "macos")]
    #[error("mirrord-layer: SIP patch failed with error `{0}`!")]
    FailedSipPatch(#[from] SipError),

    #[error("mirrord-layer: IPv6 can't be used with mirrord")]
    SocketUnsuportedIpv6,

    // `From` implemented below, not with `#[from]` so that when new variants of
    // `SerializationError` are added, they are mapped into different variants of
    // `LayerError`.
    #[error(
        "mirrord-layer: user application is trying to connect to an address that is not a \
        supported IP or unix socket address."
    )]
    UnsupportedSocketType,

    #[error("mirrord-layer: Pointer argument points to an invalid address")]
    BadPointer,

    #[error("mirrord-layer: Socket address `{0}` is already bound!")]
    AddressAlreadyBound(SocketAddr),

    /// When the user's application tries to access a file filtered out by the `not-found` file
    /// filter.
    #[error("mirrord-layer: Ignored file")]
    FileNotFound,

    #[error("mirrord-layer: Proxy connection failed: `{0}`")]
    ProxyError(#[from] ProxyError),
}

/// Errors internal to mirrord-layer.
///
/// You'll encounter these when the layer is performing some of its internal operations, mostly when
/// handling [`ProxyToLayerMessage`](mirrord_intproxy::protocol::ProxyToLayerMessage).
#[derive(Error, Debug)]
pub(crate) enum LayerError {
    #[error("mirrord-layer: Failed while getting a response!")]
    ResponseError(#[from] ResponseError),

    #[error("mirrord-layer: Frida failed with `{0}`!")]
    Frida(frida_gum::Error),

    #[error("mirrord-layer: Failed to find export for name `{0}`!")]
    NoExportName(String),

    #[cfg(target_os = "linux")]
    #[error("mirrord-layer: Failed to find symbol for name `{0}`!")]
    NoSymbolName(String),

    #[error("mirrord-layer: Environment variable interaction failed with `{0}`!")]
    VarError(#[from] VarError),

    #[error("mirrord-layer: Parsing `bool` value failed with `{0}`!")]
    ParseBoolError(#[from] ParseBoolError),

    #[error("mirrord-layer: IO failed with `{0}`!")]
    IO(#[from] std::io::Error),

    #[error("mirrord-layer: JSON convert error")]
    JSONConvertError(#[from] serde_json::Error),

    #[error("mirrord-layer: Failed setting up mirrord with configuration error `{0}`!")]
    Config(#[from] ConfigError),

    #[error("mirrord-layer: Failed to find a process to hook mirrord into!")]
    NoProcessFound,

    #[error("mirrord-layer: Regex creation failed with `{0}`.")]
    Regex(#[from] fancy_regex::Error),

    // `From` implemented below, not with `#[from]` so that when new variants of
    // `SerializationError` are added, they are mapped into different variants of
    // `LayerError`.
    #[error(
        "mirrord-layer: user application is trying to connect to an address that is not a \
        supported IP or unix socket address."
    )]
    UnsupportedSocketType,

    #[cfg(target_os = "macos")]
    #[error("Exec failed with error {0:?}, please report this error!")]
    ExecFailed(exec::Error),

    #[error("mirrord-layer: Something went wrong with the outgoing filter `{0}`.")]
    OutgoingFilterError(#[from] OutgoingFilterError),
}

impl From<SerializationError> for LayerError {
    fn from(err: SerializationError) -> Self {
        match err {
            SerializationError::SocketAddress => LayerError::UnsupportedSocketType,
        }
    }
}

impl From<SerializationError> for HookError {
    fn from(err: SerializationError) -> Self {
        match err {
            SerializationError::SocketAddress => HookError::UnsupportedSocketType,
        }
    }
}

// Cannot have a generic `From<T>` implementation for this error, so explicitly implemented here.
impl<'a, T> From<std::sync::PoisonError<std::sync::MutexGuard<'a, T>>> for HookError {
    fn from(_: std::sync::PoisonError<std::sync::MutexGuard<T>>) -> Self {
        HookError::LockError
    }
}

pub(crate) type Result<T, E = LayerError> = std::result::Result<T, E>;
pub(crate) type HookResult<T, E = HookError> = std::result::Result<T, E>;

/// mapping based on - <https://man7.org/linux/man-pages/man3/errno.3.html>
impl From<HookError> for i64 {
    fn from(fail: HookError) -> Self {
        match fail {
            HookError::AddressAlreadyBound(_)
            | HookError::ResponseError(
                ResponseError::NotFound(_)
                | ResponseError::NotFile(_)
                | ResponseError::NotDirectory(_)
                | ResponseError::Remote(_)
                | ResponseError::RemoteIO(_)
                | ResponseError::DnsLookup(_),
            ) => {
                info!("libc error (doesn't indicate a problem) >> {fail:#?}")
            }
            HookError::IO(ref e) if (is_ignored_code(e.raw_os_error())) => {
                info!("libc error (doesn't indicate a problem) >> {fail:#?}")
            }
            HookError::FileNotFound => {
                info!("mirrord file not found triggered")
            }
            HookError::SocketUnsuportedIpv6 => {
                info!("{fail}")
            }
            _ => error!("Error occured in Layer >> {fail:?}"),
        };

        let libc_error = match fail {
            HookError::Null(_) => libc::EINVAL,
            HookError::TryFromInt(_) => libc::EINVAL,
            HookError::CannotGetProxyConnection => libc::EINVAL,
            HookError::ProxyError(_) => libc::EINVAL,
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
                ResponseError::DnsLookup(dns_fail) => {
                    return match dns_fail.kind {
                        mirrord_protocol::ResolveErrorKindInternal::Timeout => libc::EAI_AGAIN,
                        // prevents an infinite loop that used to happen in some apps, don't know if
                        // this is the correct mapping.
                        mirrord_protocol::ResolveErrorKindInternal::NoRecordsFound(_) => {
                            libc::EAI_NONAME
                        }
                        _ => libc::EAI_FAIL,
                        // TODO: Add more error kinds, next time we break protocol compatibility.
                    } as _;
                }
                // for listen, EINVAL means "socket is already connected."
                // Will not happen, because this ResponseError is not return from any hook, so it
                // never appears as HookError::ResponseError(PortAlreadyStolen(_)).
                // this could be changed by waiting for the Subscribed response from agent.
                ResponseError::PortAlreadyStolen(_port) => libc::EINVAL,
                ResponseError::NotImplemented => libc::EINVAL,
            },
            HookError::DNSNoName => libc::EFAULT,
            HookError::Utf8(_) => libc::EINVAL,
            HookError::NullPointer => libc::EINVAL,
            HookError::LocalFileCreation(_) => libc::EINVAL,
            #[cfg(target_os = "macos")]
            HookError::FailedSipPatch(_) => libc::EACCES,
            HookError::SocketUnsuportedIpv6 => libc::EAFNOSUPPORT,
            HookError::UnsupportedSocketType => libc::EAFNOSUPPORT,
            HookError::BadPointer => libc::EFAULT,
            HookError::AddressAlreadyBound(_) => libc::EADDRINUSE,
            HookError::FileNotFound => libc::ENOENT,
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

impl From<HookError> for *mut DIR {
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

impl From<frida_gum::Error> for LayerError {
    fn from(err: frida_gum::Error) -> Self {
        LayerError::Frida(err)
    }
}

impl From<HookError> for *mut hostent {
    fn from(_fail: HookError) -> Self {
        ptr::null_mut()
    }
}
