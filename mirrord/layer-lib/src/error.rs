//! Cross-platform error handling for mirrord layer operations.
//!
//! This module provides error types that can be shared between the Unix layer and Windows
//! layer-win, along with platform-specific conversions to native error codes.

#[cfg(target_os = "windows")]
pub mod windows;

#[cfg(unix)]
use std::ptr;
use std::{
    env::VarError,
    io,
    net::{AddrParseError, SocketAddr},
    str::ParseBoolError,
    sync::PoisonError,
};

// TODO: These imports need proper dependencies or should be moved to the layer crate
#[cfg(target_os = "macos")]
use exec;
#[cfg(target_os = "macos")]
use libc::EACCES;
#[cfg(unix)]
use libc::{
    DIR, EADDRINUSE, EAFNOSUPPORT, EAI_AGAIN, EAI_FAIL, EAI_NONAME, EBADF, EFAULT, EINVAL, EIO,
    EISDIR, ENETUNREACH, ENOENT, ENOMEM, ENOTDIR, FILE, c_char, hostent,
};
use mirrord_config::config::ConfigError;
use mirrord_intproxy_protocol::{ProxyToLayerMessage, codec::CodecError};
use mirrord_protocol::{ResponseError, SerializationError};
#[cfg(target_os = "macos")]
use mirrord_sip::SipError;
#[cfg(unix)]
use nix::errno::Errno;
use thiserror::Error;
use tracing::{error, info};
#[cfg(windows)]
use winapi::{
    shared::winerror::{ERROR_DIRECTORY, ERROR_FILE_NOT_FOUND, ERROR_IO_DEVICE},
    um::winsock2::{
        WSAEADDRINUSE, WSAEAFNOSUPPORT, WSAEBADF, WSAEFAULT, WSAEINVAL, WSAENETUNREACH, WSAENOBUFS,
        WSAHOST_NOT_FOUND, WSANO_RECOVERY, WSATRY_AGAIN,
    },
};

#[cfg(target_os = "windows")]
use crate::error::windows::ConsoleError;
use crate::graceful_exit;

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
        winapi::um::winsock2::WSAEINPROGRESS,
        winapi::um::winsock2::WSAEAFNOSUPPORT,
        winapi::um::winsock2::WSAEADDRINUSE,
        // Using EACCES as equivalent to EPERM
        winapi::um::winsock2::WSAEACCES,
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

#[derive(Debug, Error)]
pub enum ProxyError {
    #[error("{0}")]
    CodecError(#[from] CodecError),
    #[error("connection closed")]
    ConnectionClosed,
    #[error("unexpected response: {0:?}")]
    UnexpectedResponse(ProxyToLayerMessage),
    #[error("critical error: {0}")]
    ProxyFailure(String),
    #[error("connection lock poisoned")]
    LockPoisoned,
    #[error("{0}")]
    IoFailed(#[from] io::Error),
}

impl<T> From<PoisonError<T>> for ProxyError {
    fn from(_value: PoisonError<T>) -> Self {
        Self::LockPoisoned
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

/// Errors that can occur during hostname resolution operations.
///
/// This error type covers various failure modes when resolving hostnames,
/// reading configuration files, or performing DNS operations.
#[derive(Error, Debug)]
pub enum HostnameResolveError {
    #[error("Proxy connection not available")]
    ProxyConnectionUnavailable,

    #[error("Failed to open file {path}: {details}")]
    FileOpenError { path: String, details: &'static str },

    #[error("Failed to read file {path}: {details}")]
    FileReadError { path: String, details: &'static str },

    #[error("Remote error while accessing file {path}: {error}")]
    RemoteFileError { path: String, error: &'static str },

    #[error("DNS resolution failed for hostname '{hostname}': {details}")]
    DnsResolutionFailed {
        hostname: String,
        details: &'static str,
    },

    #[error("Invalid hostname: '{hostname}' (reason: {reason})")]
    InvalidHostname {
        hostname: String,
        reason: &'static str,
    },

    #[error("Configuration parsing error in {file}: {details}")]
    ConfigParseError { file: String, details: &'static str },

    #[error("No hostname available from any source")]
    NoHostnameAvailable,

    #[error("Cache lock error: {details}")]
    CacheLockError { details: &'static str },
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

    #[error("mirrord-layer: Hostname resolution failed with `{0}`!")]
    HostnameResolveError(#[from] HostnameResolveError),
}

/// Errors internal to mirrord-layer.
///
/// You'll encounter these when the layer is performing some of its internal operations, mostly when
/// handling [`ProxyToLayerMessage`].
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
    #[allow(dead_code)]
    #[error("Exec failed with error {0:?}, please report this error!")]
    ExecFailed(exec::Error),

    #[error("Failed applying hook for function {0}, dll {1}")]
    FailedApplyingAPIHook(String, String),

    #[error("Environment variable for intproxy address is missing: {0}")]
    MissingEnvIntProxyAddr(VarError),
    #[error("Intproxy address malformed: {0}")]
    MalformedIntProxyAddr(AddrParseError),

    #[error("Proxy connection failed: {0}")]
    ProxyConnectionFailed(#[from] ProxyError),

    #[error("Global {0} already initialized")]
    GlobalAlreadyInitialized(&'static str),

    #[cfg(target_os = "windows")]
    #[error("Console failure")]
    WindowsConsoleError(#[from] ConsoleError),
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

/// Enables efficient error handling with boxed HookError for memory optimization
impl From<Box<HookError>> for HookError {
    fn from(boxed_error: Box<HookError>) -> Self {
        *boxed_error
    }
}

#[cfg(unix)]
impl From<frida_gum::Error> for LayerError {
    fn from(err: frida_gum::Error) -> Self {
        LayerError::Frida(err)
    }
}

pub type LayerResult<T, E = LayerError> = std::result::Result<T, E>;
pub type HookResult<T, E = HookError> = std::result::Result<T, E>;
pub type ProxyResult<T, E = ProxyError> = std::result::Result<T, E>;

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
                match err {
                    ProxyError::ProxyFailure(err) => {
                        graceful_exit!("Proxy encountered an error: {err}")
                    }
                    err => {
                        graceful_exit!("Proxy error, connectivity issue or a bug: {err}")
                    }
                };
            }
            #[cfg(target_os = "macos")]
            HookError::FailedSipPatch(_) => {
                error!("SIP patch failed >> {fail:?}")
            }
            _ => error!("Error occured in Layer >> {fail:?}"),
        };

        let errno = match fail {
            HookError::Null(_) => {
                #[cfg(unix)]
                {
                    EINVAL
                }
                #[cfg(windows)]
                {
                    WSAEINVAL
                }
            }
            HookError::TryFromInt(_) => {
                #[cfg(unix)]
                {
                    EINVAL
                }
                #[cfg(windows)]
                {
                    WSAEINVAL
                }
            }
            HookError::CannotGetProxyConnection => {
                #[cfg(unix)]
                {
                    EINVAL
                }
                #[cfg(windows)]
                {
                    WSAEINVAL
                }
            }
            HookError::ProxyError(_) => {
                #[cfg(unix)]
                {
                    EINVAL
                }
                #[cfg(windows)]
                {
                    WSAEINVAL
                }
            }
            HookError::IO(io_fail) => {
                #[cfg(unix)]
                {
                    io_fail.raw_os_error().unwrap_or(EIO)
                }
                #[cfg(windows)]
                {
                    io_fail.raw_os_error().unwrap_or(ERROR_IO_DEVICE as i32)
                }
            }
            HookError::LockError => {
                #[cfg(unix)]
                {
                    EINVAL
                }
                #[cfg(windows)]
                {
                    WSAEINVAL
                }
            }
            HookError::BincodeEncode(_) => {
                #[cfg(unix)]
                {
                    EINVAL
                }
                #[cfg(windows)]
                {
                    WSAEINVAL
                }
            }
            HookError::ResponseError(response_fail) => match response_fail {
                ResponseError::IdsExhausted(_) => {
                    #[cfg(unix)]
                    {
                        ENOMEM
                    }
                    #[cfg(windows)]
                    {
                        WSAENOBUFS
                    }
                }
                ResponseError::OpenLocal => {
                    #[cfg(unix)]
                    {
                        ENOENT
                    }
                    #[cfg(windows)]
                    {
                        ERROR_FILE_NOT_FOUND as i32
                    }
                }
                ResponseError::NotFound(_) => {
                    #[cfg(unix)]
                    {
                        ENOENT
                    }
                    #[cfg(windows)]
                    {
                        ERROR_FILE_NOT_FOUND as i32
                    }
                }
                ResponseError::NotDirectory(_) => {
                    #[cfg(unix)]
                    {
                        ENOTDIR
                    }
                    #[cfg(windows)]
                    {
                        ERROR_DIRECTORY as i32
                    }
                }
                ResponseError::NotFile(_) => {
                    #[cfg(unix)]
                    {
                        EISDIR
                    }
                    #[cfg(windows)]
                    {
                        ERROR_DIRECTORY as i32
                    }
                }
                ResponseError::RemoteIO(io_fail) => {
                    #[cfg(unix)]
                    {
                        io_fail.raw_os_error.unwrap_or(EIO)
                    }
                    #[cfg(windows)]
                    {
                        io_fail.raw_os_error.unwrap_or(ERROR_IO_DEVICE as i32)
                    }
                }
                ResponseError::Remote(remote) => match remote {
                    // So far only encountered when trying to make requests from golang.
                    mirrord_protocol::RemoteError::ConnectTimedOut(_) => {
                        #[cfg(unix)]
                        {
                            ENETUNREACH
                        }
                        #[cfg(windows)]
                        {
                            WSAENETUNREACH
                        }
                    }
                    _ => {
                        #[cfg(unix)]
                        {
                            EINVAL
                        }
                        #[cfg(windows)]
                        {
                            WSAEINVAL
                        }
                    }
                },
                ResponseError::DnsLookup(dns_fail) => {
                    return match dns_fail.kind {
                        mirrord_protocol::ResolveErrorKindInternal::Timeout => {
                            #[cfg(unix)]
                            {
                                EAI_AGAIN
                            }
                            #[cfg(windows)]
                            {
                                WSATRY_AGAIN
                            }
                        }
                        // prevents an infinite loop that used to happen in some apps, don't know if
                        // this is the correct mapping.
                        mirrord_protocol::ResolveErrorKindInternal::NoRecordsFound(_) => {
                            #[cfg(unix)]
                            {
                                EAI_NONAME
                            }
                            #[cfg(windows)]
                            {
                                WSAHOST_NOT_FOUND
                            }
                        }
                        _ => {
                            #[cfg(unix)]
                            {
                                EAI_FAIL
                            }
                            #[cfg(windows)]
                            {
                                WSANO_RECOVERY
                            }
                        } // TODO: Add more error kinds, next time we break protocol compatibility.
                    } as _;
                }
                // for listen, EINVAL means "socket is already connected."
                // Will not happen, because this ResponseError is not return from any hook, so it
                // never appears as HookError::ResponseError(PortAlreadyStolen(_)).
                // this could be changed by waiting for the Subscribed response from agent.
                ResponseError::PortAlreadyStolen(_port) => {
                    #[cfg(unix)]
                    {
                        EINVAL
                    }
                    #[cfg(windows)]
                    {
                        WSAEINVAL
                    }
                }
                ResponseError::NotImplemented => {
                    #[cfg(unix)]
                    {
                        EINVAL
                    }
                    #[cfg(windows)]
                    {
                        WSAEINVAL
                    }
                }
                ResponseError::StripPrefix(_) => {
                    #[cfg(unix)]
                    {
                        EINVAL
                    }
                    #[cfg(windows)]
                    {
                        WSAEINVAL
                    }
                }
                ResponseError::Forbidden { .. } | ResponseError::ForbiddenWithReason { .. } => {
                    graceful_exit!("Operation forbidden by policy");
                }
            },
            HookError::DNSNoName => {
                #[cfg(unix)]
                {
                    EFAULT
                }
                #[cfg(windows)]
                {
                    WSAEFAULT
                }
            }
            HookError::Utf8(_) => {
                #[cfg(unix)]
                {
                    EINVAL
                }
                #[cfg(windows)]
                {
                    WSAEINVAL
                }
            }
            HookError::NullPointer => {
                #[cfg(unix)]
                {
                    EINVAL
                }
                #[cfg(windows)]
                {
                    WSAEINVAL
                }
            }
            HookError::LocalFileCreation(_, err) => err,
            #[cfg(target_os = "macos")]
            HookError::FailedSipPatch(_) => EACCES,
            HookError::SocketUnsuportedIpv6 => {
                #[cfg(unix)]
                {
                    EAFNOSUPPORT
                }
                #[cfg(windows)]
                {
                    WSAEAFNOSUPPORT
                }
            }
            HookError::UnsupportedSocketType => {
                #[cfg(unix)]
                {
                    EAFNOSUPPORT
                }
                #[cfg(windows)]
                {
                    WSAEAFNOSUPPORT
                }
            }
            HookError::BadPointer => {
                #[cfg(unix)]
                {
                    EFAULT
                }
                #[cfg(windows)]
                {
                    WSAEFAULT
                }
            }
            HookError::AddressAlreadyBound(_) => {
                #[cfg(unix)]
                {
                    EADDRINUSE
                }
                #[cfg(windows)]
                {
                    WSAEADDRINUSE
                }
            }
            HookError::FileNotFound => {
                #[cfg(unix)]
                {
                    ENOENT
                }
                #[cfg(windows)]
                {
                    ERROR_FILE_NOT_FOUND as i32
                }
            }
            #[cfg(target_os = "linux")]
            HookError::BadDescriptor => EBADF,
            #[cfg(target_os = "linux")]
            HookError::BadFlag => EINVAL,
            #[cfg(target_os = "linux")]
            HookError::EmptyPath => ENOENT,
            HookError::InvalidBindAddressForDomain => {
                #[cfg(unix)]
                {
                    EINVAL
                }
                #[cfg(windows)]
                {
                    WSAEINVAL
                }
            }
            HookError::SocketNotFound(_) => {
                #[cfg(unix)]
                {
                    EBADF
                }
                #[cfg(windows)]
                {
                    WSAEBADF
                }
            }
            HookError::ManagedSocketNotFound(_) => {
                #[cfg(unix)]
                {
                    EBADF
                }
                #[cfg(windows)]
                {
                    WSAEBADF
                }
            }
            HookError::ConnectError(_) => {
                #[cfg(unix)]
                {
                    EFAULT
                }
                #[cfg(windows)]
                {
                    WSAEFAULT
                }
            }
            HookError::AddrInfoError(_) => {
                #[cfg(unix)]
                {
                    EFAULT
                }
                #[cfg(windows)]
                {
                    WSAEFAULT
                }
            }
            HookError::SendToError(_) => {
                #[cfg(unix)]
                {
                    EFAULT
                }
                #[cfg(windows)]
                {
                    WSAEFAULT
                }
            }
            HookError::HostnameResolveError(_) => {
                #[cfg(unix)]
                {
                    EFAULT
                }
                #[cfg(windows)]
                {
                    WSAEFAULT
                }
            }
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
