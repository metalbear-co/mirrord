//! Cross-platform error handling for mirrord layer operations.
//!
//! This module provides error types that can be shared between the Unix layer and Windows
//! layer-win, along with platform-specific conversions to native error codes.

use std::{env::VarError, net::SocketAddr, str::ParseBoolError};

#[cfg(unix)]
use libc::{DIR, FILE, c_char, hostent};
use mirrord_config::config::ConfigError;
use mirrord_protocol::{ResponseError, SerializationError};
#[cfg(target_os = "macos")]
use mirrord_sip::SipError;
#[cfg(unix)]
use nix::errno::Errno;
use thiserror::Error;
use tracing::{error, info};

// Import ProxyError and SipError (conditionally)
use crate::proxy_connection::ProxyError;

mod ignore_codes {
    //! Private module for preventing access to the [`IGNORE_ERROR_CODES`] constant.

    /// Error codes from [`libc`] that are **not** hard errors, meaning the operation may progress.
    ///
    /// Prefer using [`is_ignored_code`] instead of relying on this constant.
    #[cfg(unix)]
    const IGNORE_ERROR_CODES: [i32; 4] = [
        libc::EINPROGRESS,
        libc::EAFNOSUPPORT,
        libc::EADDRINUSE,
        libc::EPERM,
    ];

    #[cfg(windows)]
    const IGNORE_ERROR_CODES: [i32; 4] = [
        winapi::um::winsock2::WSAEINPROGRESS as i32,
        winapi::um::winsock2::WSAEAFNOSUPPORT as i32,
        winapi::um::winsock2::WSAEADDRINUSE as i32,
        winapi::um::winsock2::WSAEACCES as i32, // Using EACCES as equivalent to EPERM
    ];

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
/// Error types for connect operations
#[derive(Debug, thiserror::Error)]
pub enum AddrInfoError {
    #[error("Null pointer found")]
    NullPointer,
    #[error("Failed to allocate memory for address info")]
    AllocationFailed,
    #[error("Failed to allocate memory for layout")]
    LayoutError(#[from] std::alloc::LayoutError),
    #[error("Should use local DNS resolution for {0}")]
    ResolveDisabled(String),
    #[error("DNS lookup failed")]
    LookupFailed(#[from] std::io::Error),
    #[error("No addresses in DNS response")]
    LookupEmpty,
}

/// Error types for connect operations
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Address conversion error")]
    AddressConversion,
    #[error("Proxy request failed: {0}")]
    ProxyRequest(String),
    #[error("Fallback to original connection")]
    Fallback,
    #[error("Disabled outgoing connection")]
    DisabledOutgoing(usize),
    #[error("Bypass port: {0}")]
    BypassPort(u16),
    #[error("Parameter missing: {0}")]
    ParameterMissing(String),
    #[error("Address unreachable")]
    AddressUnreachable(String),
}

/// Error types for sendto operations
#[derive(Debug, thiserror::Error)]
pub enum SendToError {
    #[error("Address conversion error")]
    AddressConversion,
    #[error("Unix socket not supported without connect")]
    UnixSocketNotConnected,
    #[error("Mutex lock error")]
    LockError,
    #[error("Should bypass to original function")]
    Bypass,
    #[error("Send failed")]
    SendFailed(isize),
}

/// Errors that occur in the layer's hook functions, and will reach the user's application.
///
/// These errors are converted to [`libc`] error codes, and are also used to [`Errno::set_raw`].
#[derive(Error, Debug)]
pub enum HookError {
    #[error("mirrord-layer: `{0}`")]
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

    #[error("mirrord-layer: Failed creating local file for `remote_fd` `{0}`! with errno `{1}`")]
    LocalFileCreation(u64, i32),

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

    #[cfg(target_os = "linux")]
    #[error("mirrord-layer: Invalid descriptor argument")]
    BadDescriptor,

    #[cfg(target_os = "linux")]
    #[error("mirrord-layer: Bad flag passed in argument")]
    BadFlag,

    #[cfg(target_os = "linux")]
    #[error("mirrord-layer: Empty file path passed in argument")]
    EmptyPath,

    #[error("mirrord-layer: address passed to `bind` is not valid for the socket domain")]
    InvalidBindAddressForDomain,

    #[error("mirrord-layer: Failed encoding value with `{0}`!")]
    BincodeEncode(#[from] bincode::error::EncodeError),

    #[error("mirrord-layer: Socket not found `{0}`!")]
    SocketNotFound(usize),

    #[error("mirrord-layer: Managed Socket not found on address `{0}`!")]
    ManagedSocketNotFound(SocketAddr),

    #[error("mirrord-layer: Managed Socket not found on address `{0}`!")]
    ConnectError(#[from] ConnectError),

    #[error("mirrord-layer: Address resolution failed with `{0}`!")]
    AddrInfoError(#[from] AddrInfoError),

    #[error("mirrord-layer: SendTo failed with `{0}`!")]
    SendToError(#[from] SendToError),
}

/// Errors internal to mirrord-layer.
///
/// You'll encounter these when the layer is performing some of its internal operations, mostly when
/// handling [`ProxyToLayerMessage`](mirrord_intproxy_protocol::ProxyToLayerMessage).
#[derive(Error, Debug)]
pub enum LayerError {
    #[error("mirrord-layer: `{0}`")]
    ResponseError(#[from] ResponseError),

    #[cfg(unix)]
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

    #[error("mirrord-layer: Failed setting up mirrord with configuration error `{0}`!")]
    Config(#[from] ConfigError),

    #[cfg(unix)]
    #[error("mirrord-layer: Failed to find a process to hook mirrord into!")]
    NoProcessFound,

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
impl<T> From<std::sync::PoisonError<std::sync::MutexGuard<'_, T>>> for HookError {
    fn from(_: std::sync::PoisonError<std::sync::MutexGuard<T>>) -> Self {
        HookError::LockError
    }
}

#[cfg(unix)]
impl From<frida_gum::Error> for LayerError {
    fn from(err: frida_gum::Error) -> Self {
        LayerError::Frida(err)
    }
}

pub type Result<T, E = LayerError> = std::result::Result<T, E>;
pub type HookResult<T, E = HookError> = std::result::Result<T, E>;

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
            HookError::IO(ref e) if (ignore_codes::is_ignored_code(e.raw_os_error())) => {
                info!("libc error (doesn't indicate a problem) >> {fail:#?}")
            }
            HookError::FileNotFound => {
                info!("mirrord file not found triggered")
            }
            HookError::SocketUnsuportedIpv6 => {
                info!("{fail}")
            }
            HookError::ResponseError(ResponseError::NotImplemented) => {
                // this means we bypass, so we can just return to avoid setting libc.
                return -1;
            }
            HookError::ProxyError(ref err) => {
                let reason = match err {
                    ProxyError::ProxyFailure(err) => {
                        format!("Proxy encountered an error: {err}")
                    }
                    err => format!("Proxy error, connectivity issue or a bug: {err}"),
                };
                // For cross-platform compatibility, we can't use graceful_exit! macro here
                // Instead, just log and continue with error mapping
                error!("Proxy error: {reason}");
            }
            _ => error!("Error occured in Layer >> {fail:?}"),
        };

        let errno = match fail {
            HookError::Null(_) => platform_errors::EINVAL,
            HookError::TryFromInt(_) => platform_errors::EINVAL,
            HookError::CannotGetProxyConnection => platform_errors::EINVAL,
            HookError::ProxyError(_) => platform_errors::EINVAL,
            HookError::IO(io_fail) => io_fail.raw_os_error().unwrap_or(platform_errors::EIO),
            HookError::LockError => platform_errors::EINVAL,
            HookError::BincodeEncode(_) => platform_errors::EINVAL,
            HookError::ResponseError(response_fail) => match response_fail {
                ResponseError::IdsExhausted(_) => platform_errors::ENOMEM,
                ResponseError::OpenLocal => platform_errors::ENOENT,
                ResponseError::NotFound(_) => platform_errors::ENOENT,
                ResponseError::NotDirectory(_) => platform_errors::ENOTDIR,
                ResponseError::NotFile(_) => platform_errors::EISDIR,
                ResponseError::RemoteIO(io_fail) => {
                    io_fail.raw_os_error.unwrap_or(platform_errors::EIO)
                }
                ResponseError::Remote(remote) => match remote {
                    // So far only encountered when trying to make requests from golang.
                    mirrord_protocol::RemoteError::ConnectTimedOut(_) => {
                        platform_errors::ENETUNREACH
                    }
                    _ => platform_errors::EINVAL,
                },
                ResponseError::DnsLookup(dns_fail) => {
                    return match dns_fail.kind {
                        mirrord_protocol::ResolveErrorKindInternal::Timeout => {
                            platform_errors::EAI_AGAIN
                        }
                        // prevents an infinite loop that used to happen in some apps, don't know if
                        // this is the correct mapping.
                        mirrord_protocol::ResolveErrorKindInternal::NoRecordsFound(_) => {
                            platform_errors::EAI_NONAME
                        }
                        _ => platform_errors::EAI_FAIL,
                        // TODO: Add more error kinds, next time we break protocol compatibility.
                    } as _;
                }
                // for listen, EINVAL means "socket is already connected."
                // Will not happen, because this ResponseError is not return from any hook, so it
                // never appears as HookError::ResponseError(PortAlreadyStolen(_)).
                // this could be changed by waiting for the Subscribed response from agent.
                ResponseError::PortAlreadyStolen(_port) => platform_errors::EINVAL,
                ResponseError::NotImplemented => platform_errors::EINVAL,
                ResponseError::StripPrefix(_) => platform_errors::EINVAL,
                ResponseError::Forbidden { .. } | ResponseError::ForbiddenWithReason { .. } => {
                    // For cross-platform compatibility, we can't use graceful_exit! macro here
                    error!("Operation forbidden by policy");
                    platform_errors::EACCES
                }
            },
            HookError::DNSNoName => platform_errors::EFAULT,
            HookError::Utf8(_) => platform_errors::EINVAL,
            HookError::NullPointer => platform_errors::EINVAL,
            HookError::LocalFileCreation(_, err) => err,
            #[cfg(target_os = "macos")]
            HookError::FailedSipPatch(_) => platform_errors::EACCES,
            HookError::SocketUnsuportedIpv6 => platform_errors::EAFNOSUPPORT,
            HookError::UnsupportedSocketType => platform_errors::EAFNOSUPPORT,
            HookError::BadPointer => platform_errors::EFAULT,
            HookError::AddressAlreadyBound(_) => platform_errors::EADDRINUSE,
            HookError::FileNotFound => platform_errors::ENOENT,
            #[cfg(target_os = "linux")]
            HookError::BadDescriptor => platform_errors::EBADF,
            #[cfg(target_os = "linux")]
            HookError::BadFlag => platform_errors::EINVAL,
            #[cfg(target_os = "linux")]
            HookError::EmptyPath => platform_errors::ENOENT,
            HookError::InvalidBindAddressForDomain => platform_errors::EINVAL,
            HookError::SocketNotFound(_) => platform_errors::EBADF,
            HookError::ManagedSocketNotFound(_) => platform_errors::EBADF,
            HookError::ConnectError(_) => platform_errors::EFAULT,
            HookError::AddrInfoError(_) => platform_errors::EFAULT,
            HookError::SendToError(_) => platform_errors::EFAULT,
        };

        // Set platform-specific error
        #[cfg(unix)]
        {
            Errno::set_raw(errno);
        }
        #[cfg(windows)]
        {
            unsafe {
                winapi::um::errhandlingapi::SetLastError(errno as u32);
            }
        }

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

#[cfg(unix)]
impl From<HookError> for *mut FILE {
    fn from(fail: HookError) -> Self {
        let _ = i64::from(fail);

        ptr::null_mut()
    }
}

#[cfg(unix)]
impl From<HookError> for *mut DIR {
    fn from(fail: HookError) -> Self {
        let _ = i64::from(fail);

        ptr::null_mut()
    }
}

#[cfg(unix)]
impl From<HookError> for *mut c_char {
    fn from(fail: HookError) -> Self {
        let _ = i64::from(fail);

        ptr::null_mut()
    }
}

#[cfg(unix)]
impl From<HookError> for *mut hostent {
    fn from(_fail: HookError) -> Self {
        ptr::null_mut()
    }
}

// Platform-specific error constants
#[cfg(unix)]
mod platform_errors {
    pub const EINVAL: i32 = libc::EINVAL;
    pub const ENOENT: i32 = libc::ENOENT;
    pub const ENOTDIR: i32 = libc::ENOTDIR;
    pub const EISDIR: i32 = libc::EISDIR;
    pub const EIO: i32 = libc::EIO;
    pub const ENOMEM: i32 = libc::ENOMEM;
    pub const ENETUNREACH: i32 = libc::ENETUNREACH;
    pub const EFAULT: i32 = libc::EFAULT;
    pub const EAFNOSUPPORT: i32 = libc::EAFNOSUPPORT;
    pub const EADDRINUSE: i32 = libc::EADDRINUSE;
    pub const EBADF: i32 = libc::EBADF;
    pub const EACCES: i32 = libc::EACCES;
    pub const EAI_AGAIN: i32 = libc::EAI_AGAIN;
    pub const EAI_NONAME: i32 = libc::EAI_NONAME;
    pub const EAI_FAIL: i32 = libc::EAI_FAIL;
}

#[cfg(windows)]
mod platform_errors {
    use winapi::{
        shared::winerror::{ERROR_DIRECTORY, ERROR_FILE_NOT_FOUND, ERROR_IO_DEVICE},
        um::winsock2::{
            WSAEACCES, WSAEADDRINUSE, WSAEAFNOSUPPORT, WSAEBADF, WSAEFAULT, WSAEINVAL,
            WSAENETUNREACH, WSAENOBUFS, WSAHOST_NOT_FOUND, WSANO_RECOVERY, WSATRY_AGAIN,
        },
    };

    pub const EINVAL: i32 = WSAEINVAL as i32;
    pub const ENOENT: i32 = ERROR_FILE_NOT_FOUND as i32;
    pub const ENOTDIR: i32 = ERROR_DIRECTORY as i32;
    pub const EISDIR: i32 = ERROR_DIRECTORY as i32;
    pub const EIO: i32 = ERROR_IO_DEVICE as i32;
    pub const ENOMEM: i32 = WSAENOBUFS as i32;
    pub const ENETUNREACH: i32 = WSAENETUNREACH as i32;
    pub const EFAULT: i32 = WSAEFAULT as i32;
    pub const EAFNOSUPPORT: i32 = WSAEAFNOSUPPORT as i32;
    pub const EADDRINUSE: i32 = WSAEADDRINUSE as i32;
    pub const EBADF: i32 = WSAEBADF as i32;
    pub const EACCES: i32 = WSAEACCES as i32;
    pub const EAI_AGAIN: i32 = WSATRY_AGAIN as i32;
    pub const EAI_NONAME: i32 = WSAHOST_NOT_FOUND as i32;
    pub const EAI_FAIL: i32 = WSANO_RECOVERY as i32;
}
