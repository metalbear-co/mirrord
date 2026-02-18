//! Cross-platform error handling for mirrord layer operations.
//!
//! This module provides error types that can be shared between the Unix layer and Windows
//! layer-win, along with platform-specific conversions to native error codes.

#[cfg(windows)]
pub mod windows;

#[cfg(unix)]
use std::ptr;
use std::{
    env::VarError,
    net::{AddrParseError, SocketAddr},
    str::ParseBoolError,
    sync::{MutexGuard, PoisonError},
};

#[cfg(unix)]
use libc::{DIR, FILE, c_char, hostent};
use mirrord_config::config::ConfigError;
use mirrord_protocol::{DnsLookupError, ResponseError, SerializationError};
#[cfg(target_os = "macos")]
use mirrord_sip::SipError;
#[cfg(unix)]
use nix::errno::Errno;
use thiserror::Error;
use tracing::{error, info};
#[cfg(windows)]
use winapi::shared::winerror::{WSAHOST_NOT_FOUND, WSANO_RECOVERY, WSATRY_AGAIN};

use crate::{detour::Bypass, graceful_exit, proxy_connection::ProxyError, setup::setup};

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
        winapi::um::winsock2::WSAEACCES, // Using EACCES as equivalent to EPERM
    ];

    /// Checks if an error code from some function should be treated as a hard error, or
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
    DisabledOutgoing(i64),
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
    FileOpenError { path: String, details: String },

    #[error("Failed to read file {path}: {details}")]
    FileReadError { path: String, details: String },

    #[error("DNS resolution failed for hostname '{hostname}': {details}")]
    DnsResolutionFailed { hostname: String, details: String },

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
/// These errors are converted to `libc` error codes, and are also used to `Errno::set_raw`.
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

    #[error("mirrord-layer: IPv6 support disabled")]
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
    #[error("mirrord-layer: Ignored file `{0}`")]
    FileNotFound(String),

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
    SocketNotFound(i64),

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

    #[error("mirrord-layer: Hook bypassed")]
    Bypass(Bypass),
}

// mirrord/layer-lib/src/error.rs
impl From<Bypass> for HookError {
    fn from(bypass: Bypass) -> Self {
        HookError::Bypass(bypass)
    }
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

    #[cfg(target_os = "linux")]
    #[error("mirrord-layer: Failed to find module for name `{0}`!")]
    NoModuleName(String),

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
    #[error("Hook engine failure: {0}")]
    HookEngine(String),

    #[cfg(target_os = "windows")]
    #[error("Hook engine failure while applying {function} from {dll}: {error}")]
    HookEngineApply {
        function: &'static str,
        dll: &'static str,
        error: String,
    },

    #[cfg(target_os = "windows")]
    #[error("Detour guard failure: {0}")]
    DetourGuard(String),

    #[cfg(target_os = "windows")]
    #[error("Environment variable for layer id not present")]
    MissingLayerIdEnv,

    #[cfg(target_os = "windows")]
    #[error("Environment variable for layer id not valid u64")]
    MalformedLayerIdEnv,

    #[cfg(target_os = "windows")]
    #[error("Environment variable for config not present")]
    MissingConfigEnv,

    #[cfg(target_os = "windows")]
    #[error("Windows process creation failed: {0}")]
    WindowsProcessCreation(#[from] crate::error::windows::WindowsError),

    #[cfg(target_os = "windows")]
    #[error("DLL injection failed: {0}")]
    DllInjection(String),

    #[cfg(target_os = "windows")]
    #[error("Process synchronization failed: {0}")]
    ProcessSynchronization(String),

    #[cfg(target_os = "windows")]
    #[error("Process with PID {0} not found")]
    ProcessNotFound(u32),

    #[cfg(target_os = "windows")]
    #[error("Internal error: missing required function pointer for {0}")]
    MissingFunctionPointer(String),
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
impl<T> From<PoisonError<MutexGuard<'_, T>>> for HookError {
    fn from(_: PoisonError<MutexGuard<'_, T>>) -> Self {
        HookError::LockError
    }
}

pub type Result<T, E = LayerError> = std::result::Result<T, E>;
pub type LayerResult<T, E = LayerError> = std::result::Result<T, E>;
pub type HookResult<T, E = HookError> = std::result::Result<T, E>;

#[cfg(unix)]
fn get_platform_errno(fail: HookError) -> i32 {
    match fail {
        HookError::Null(_) => libc::EINVAL,
        HookError::TryFromInt(_) => libc::EINVAL,
        HookError::CannotGetProxyConnection => libc::EINVAL,
        HookError::ProxyError(_) => libc::EINVAL,
        HookError::IO(io_fail) => io_fail.raw_os_error().unwrap_or(libc::EIO),
        HookError::LockError => libc::EINVAL,
        HookError::BincodeEncode(_) => libc::EINVAL,
        HookError::ResponseError(response_fail) => match response_fail {
            ResponseError::IdsExhausted(_) => libc::ENOMEM,
            ResponseError::OpenLocal => libc::ENOENT,
            ResponseError::NotFound(_) => libc::ENOENT,
            ResponseError::NotDirectory(_) => libc::ENOTDIR,
            ResponseError::NotFile(_) => libc::EISDIR,
            ResponseError::RemoteIO(io_fail) => io_fail.raw_os_error.unwrap_or(libc::EIO),
            ResponseError::Remote(remote) => match remote {
                // So far only encountered when trying to make requests from golang.
                mirrord_protocol::RemoteError::ConnectTimedOut(_) => libc::ENETUNREACH,
                _ => libc::EINVAL,
            },
            ResponseError::DnsLookup(dns_fail) => translate_dns_fail(dns_fail),
            // for listen, EINVAL means "socket is already connected."
            // Will not happen, because this ResponseError is not return from any hook, so it
            // never appears as HookError::ResponseError(PortAlreadyStolen(_)).
            // this could be changed by waiting for the Subscribed response from agent.
            ResponseError::PortAlreadyStolen(_port) => libc::EINVAL,
            ResponseError::NotImplemented => libc::EINVAL,
            ResponseError::StripPrefix(_) => libc::EINVAL,
            err @ (ResponseError::Forbidden { .. } | ResponseError::ForbiddenWithReason { .. }) => {
                graceful_exit!(
                    "Stopping mirrord run. Please adjust your mirrord configuration.\n{err}"
                );
            }
        },
        HookError::DNSNoName => libc::EFAULT,
        HookError::Utf8(_) => libc::EINVAL,
        HookError::NullPointer => libc::EINVAL,
        HookError::LocalFileCreation(_, err) => err,
        #[cfg(target_os = "macos")]
        HookError::FailedSipPatch(_) => libc::EACCES,
        HookError::SocketUnsuportedIpv6 => libc::EAFNOSUPPORT,
        HookError::UnsupportedSocketType => libc::EAFNOSUPPORT,
        HookError::BadPointer => libc::EFAULT,
        HookError::AddressAlreadyBound(_) => libc::EADDRINUSE,
        HookError::FileNotFound(_) => libc::ENOENT,
        #[cfg(target_os = "linux")]
        HookError::BadDescriptor => libc::EBADF,
        #[cfg(target_os = "linux")]
        HookError::BadFlag => libc::EINVAL,
        #[cfg(target_os = "linux")]
        HookError::EmptyPath => libc::ENOENT,
        HookError::InvalidBindAddressForDomain => libc::EINVAL,
        HookError::SocketNotFound(_) => libc::EBADF,
        HookError::ManagedSocketNotFound(_) => libc::EBADF,
        HookError::ConnectError(_) => libc::EFAULT,
        HookError::AddrInfoError(_) => libc::EFAULT,
        HookError::SendToError(_) => libc::EFAULT,
        HookError::HostnameResolveError(_) => libc::EFAULT,
        // Note: temporary mapping, will be removed once Detour is migrated to windows
        HookError::Bypass(_) => libc::ENOTSUP,
    }
}

#[cfg(windows)]
fn get_platform_errno(fail: HookError) -> u32 {
    use winapi::shared::winerror::*;
    match fail {
        HookError::Null(_) => WSAEINVAL,
        HookError::TryFromInt(_) => WSAEINVAL,
        HookError::CannotGetProxyConnection => WSAEINVAL,
        HookError::ProxyError(_) => WSAEINVAL,
        HookError::IO(io_fail) => io_fail
            .raw_os_error()
            .unwrap_or(ERROR_IO_DEVICE.try_into().unwrap())
            .try_into()
            .unwrap(),
        HookError::LockError => WSAEINVAL,
        HookError::BincodeEncode(_) => WSAEINVAL,
        HookError::ResponseError(response_fail) => match response_fail {
            ResponseError::IdsExhausted(_) => WSAENOBUFS,
            ResponseError::OpenLocal => ERROR_FILE_NOT_FOUND,
            ResponseError::NotFound(_) => ERROR_FILE_NOT_FOUND,
            ResponseError::NotDirectory(_) => ERROR_DIRECTORY,
            ResponseError::NotFile(_) => ERROR_DIRECTORY,
            ResponseError::RemoteIO(io_fail) => io_fail
                .raw_os_error
                .unwrap_or(ERROR_IO_DEVICE.try_into().unwrap())
                .try_into()
                .unwrap(),
            ResponseError::Remote(remote) => match remote {
                // So far only encountered when trying to make requests from golang.
                mirrord_protocol::RemoteError::ConnectTimedOut(_) => WSAENETUNREACH,
                _ => WSAEINVAL,
            },
            ResponseError::DnsLookup(dns_fail) => {
                // try_into must work as WSAE* errors returned are actually u32
                translate_dns_fail(dns_fail).try_into().unwrap()
            }
            // for listen, EINVAL means "socket is already connected."
            // Will not happen, because this ResponseError is not return from any hook, so it
            // never appears as HookError::ResponseError(PortAlreadyStolen(_)).
            // this could be changed by waiting for the Subscribed response from agent.
            ResponseError::PortAlreadyStolen(_port) => WSAEINVAL,
            ResponseError::NotImplemented => WSAEINVAL,
            ResponseError::StripPrefix(_) => WSAEINVAL,
            err @ (ResponseError::Forbidden { .. } | ResponseError::ForbiddenWithReason { .. }) => {
                graceful_exit!(
                    "Stopping mirrord run. Please adjust your mirrord configuration.\n{err}"
                );
            }
        },
        HookError::DNSNoName => WSAEFAULT,
        HookError::Utf8(_) => WSAEINVAL,
        HookError::NullPointer => WSAEINVAL,
        HookError::LocalFileCreation(_, err) => err.try_into().unwrap(),
        #[cfg(target_os = "macos")]
        HookError::FailedSipPatch(_) => WSAEACCES,
        HookError::SocketUnsuportedIpv6 => WSAEAFNOSUPPORT,
        HookError::UnsupportedSocketType => WSAEAFNOSUPPORT,
        HookError::BadPointer => WSAEFAULT,
        HookError::AddressAlreadyBound(_) => WSAEADDRINUSE,
        HookError::FileNotFound(_) => ERROR_FILE_NOT_FOUND,
        #[cfg(target_os = "linux")]
        HookError::BadDescriptor => WSAEBADF,
        #[cfg(target_os = "linux")]
        HookError::BadFlag => WSAEINVAL,
        #[cfg(target_os = "linux")]
        HookError::EmptyPath => ERROR_FILE_NOT_FOUND,
        HookError::InvalidBindAddressForDomain => WSAEINVAL,
        HookError::SocketNotFound(_) => WSAEBADF,
        HookError::ManagedSocketNotFound(_) => WSAEBADF,
        HookError::ConnectError(_) => WSAEFAULT,
        HookError::AddrInfoError(_) => WSAEFAULT,
        HookError::SendToError(_) => WSAEFAULT,
        HookError::HostnameResolveError(_) => WSAEFAULT,
        // Note: temporary mapping, will be removed once Detour is migrated to windows
        HookError::Bypass(_) => WSAEPROTONOSUPPORT,
    }
}

fn translate_dns_fail(dns_fail: DnsLookupError) -> i32 {
    (match dns_fail.kind {
        mirrord_protocol::ResolveErrorKindInternal::Timeout => {
            #[cfg(unix)]
            {
                libc::EAI_AGAIN
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
                libc::EAI_NONAME
            }
            #[cfg(windows)]
            {
                WSAHOST_NOT_FOUND
            }
        }
        // TODO: Add more error kinds, next time we break protocol compatibility.
        _ => {
            #[cfg(unix)]
            {
                libc::EAI_FAIL
            }
            #[cfg(windows)]
            {
                WSANO_RECOVERY
            }
        }
    } as _)
}

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
                | ResponseError::RemoteIO(_),
            ) => {
                info!("libc error (doesn't indicate a problem) >> {fail:#?}")
            }
            HookError::IO(ref e) if (ignore_codes::is_ignored_code(e.raw_os_error())) => {
                info!("libc error (doesn't indicate a problem) >> {fail:#?}")
            }
            HookError::FileNotFound(ref path) => {
                info!("mirrord file not found triggered: {path}")
            }
            HookError::SocketUnsuportedIpv6 => {
                info!("{fail}")
            }
            HookError::ResponseError(ResponseError::NotImplemented) => {
                // this means we bypass, so we can just return to avoid setting libc.
                return -1;
            }
            HookError::ResponseError(ResponseError::DnsLookup(dns_fail)) => {
                // return native dns failure because their API expects the error code to be returned
                // and not reported through Errno
                return translate_dns_fail(dns_fail).into();
            }
            HookError::ProxyError(ref err) => {
                let reason = match err {
                    ProxyError::ProxyFailure(err) => {
                        format!("Proxy encountered an error: {err}")
                    }
                    err => format!("Proxy error, connectivity issue or a bug: {err}"),
                };
                graceful_exit!(
                    r"{reason}.
                    Please report it to us on https://github.com/metalbear-co/mirrord/issues/new?assignees=&labels=bug&projects=&template=bug_report.yml
                    You can find the `mirrord-intproxy` logs in {}.",
                    setup()
                        .layer_config()
                        .internal_proxy
                        .log_destination
                        .display()
                );
            }
            _ => error!("Error occured in Layer >> {fail:?}"),
        };

        // Set platform-specific error
        let errno = get_platform_errno(fail);
        #[cfg(unix)]
        Errno::set_raw(errno);

        #[cfg(windows)]
        unsafe {
            winapi::um::errhandlingapi::SetLastError(errno);
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
impl From<frida_gum::Error> for LayerError {
    fn from(err: frida_gum::Error) -> Self {
        LayerError::Frida(err)
    }
}

#[cfg(unix)]
impl From<HookError> for *mut hostent {
    fn from(_fail: HookError) -> Self {
        ptr::null_mut()
    }
}
