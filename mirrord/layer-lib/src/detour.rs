//! The layer uses features from this module to check if it should bypass one of its hooks, and call
//! the original `libc` function.
//!
//! Here we also have the convenient detour helpers that are used by the hooks to either return a
//! [`Result`]-like value, or the special [`Bypass`] case, which makes the _detour_ function call
//! the original [`libc`] equivalent, stored in a hook function pointer.
#[cfg(unix)]
use std::{cell::RefCell, ffi::CString, ops::Deref, path::PathBuf};
use std::{net::SocketAddr, sync::OnceLock};

#[cfg(target_os = "macos")]
use libc::c_char;
#[cfg(windows)]
use winapi::{
    shared::minwindef::INT,
    um::winsock2::{INVALID_SOCKET, SOCKET, WSASetLastError},
};

#[cfg(windows)]
use crate::error::get_platform_errno;
use crate::{error::HookError, socket::sockets::SocketDescriptor};

#[cfg(unix)]
thread_local!(
    /// Holds the thread-local state for bypassing the layer's detour functions.
    ///
    /// ## Warning
    ///
    /// Do **NOT** use this directly, instead use `DetourGuard::new` if you need to
    /// create a bypass inside a function (like we have in
    /// [`TcpHandler::create_local_stream`](crate::tcp::TcpHandler::create_local_stream)).
    ///
    /// Or rely on the [`hook_guard_fn`](mirrord_layer_macro::hook_guard_fn) macro.
    ///
    /// ## Details
    ///
    /// Some of the layer functions will interact with [`libc`] functions that we are hooking into,
    /// thus we could end up _stealing_ a call by the layer itself rather than by the binary the
    /// layer is injected into. An example of this  would be if we wanted to open a file locally,
    /// the layer's `open_detour` intercepts the [`libc::open`] call, and we get a remote file
    /// (if it exists), instead of the local file we wanted.
    ///
    /// We set this to `true` whenever an operation may require calling other [`libc`] functions,
    /// and back to `false` after it's done.
    static DETOUR_BYPASS: RefCell<bool> = const { RefCell::new(false) }
);

/// Sets [`DETOUR_BYPASS`] to `false`.
///
/// Prefer relying on the [`Drop`] implementation of [`DetourGuard`] instead.
#[cfg(unix)]
pub(super) fn detour_bypass_off() {
    DETOUR_BYPASS.with(|enabled| {
        if let Ok(mut bypass) = enabled.try_borrow_mut() {
            *bypass = false
        }
    });
}

/// Handler for the layer's [`DETOUR_BYPASS`].
///
/// Sets [`DETOUR_BYPASS`] on creation, and turns it off on [`Drop`].
///
/// ## Warning
///
/// You should always use `DetourGuard::new`, if you construct this in any other way, it's
/// not going to guard anything.
#[cfg(unix)]
pub struct DetourGuard;

#[cfg(unix)]
impl DetourGuard {
    /// Create a new DetourGuard if it's not already enabled.
    pub fn new() -> Option<Self> {
        DETOUR_BYPASS.with(|enabled| {
            if let Ok(bypass) = enabled.try_borrow()
                && *bypass
            {
                None
            } else {
                match enabled.try_borrow_mut() {
                    Ok(mut bypass) => {
                        *bypass = true;
                        Some(Self)
                    }
                    _ => None,
                }
            }
        })
    }
}

#[cfg(unix)]
impl Drop for DetourGuard {
    fn drop(&mut self) {
        detour_bypass_off();
    }
}

/// Wrapper around [`OnceLock`], mainly used for the [`Deref`] implementation
/// to simplify calls to the original functions as `FN_ORIGINAL()`, instead of
/// `FN_ORIGINAL.get().unwrap()`.
#[cfg(unix)]
#[derive(Debug)]
pub struct HookFn<T>(OnceLock<T>);

#[cfg(unix)]
impl<T> Deref for HookFn<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.0.get().unwrap()
    }
}

#[cfg(unix)]
impl<T> HookFn<T> {
    /// Helper function to set the inner [`OnceLock`] `T` of `self`.
    pub fn set(&self, value: T) -> Result<(), T> {
        self.0.set(value)
    }

    /// Until we can impl Default as const.
    pub const fn default_const() -> Self {
        Self(OnceLock::new())
    }
}

/// Soft-errors that can be recovered from by calling the raw FFI function.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Bypass {
    /// We're dealing with a socket port value that should be ignored, according to the incoming
    /// config.
    IgnoredInIncoming(SocketAddr),

    /// The socket type does not match one of our handled
    /// [`SocketKind`](crate::socket::SocketKind)s.
    Type(i32),

    /// Either an invalid socket domain, or one that we don't handle.
    Domain(i32),

    /// Unix socket to address that was not configured to be connected remotely.
    UnixSocket(Option<String>),

    /// We could not find this [`SocketDescriptor`] in the bookkeeping tables (`OPEN_FILES` and
    /// [`SOCKETS`](crate::socket::SOCKETS)).
    LocalFdNotFound(SocketDescriptor),

    /// Similar to `LocalFdNotFound`, but for the layer's `OPEN_DIRS` table.
    LocalDirStreamNotFound(usize),

    /// A conversion from [`SockAddr`](socket2::SockAddr) to
    /// [`SocketAddr`] failed.
    AddressConversion,

    /// The socket [`SocketDescriptor`] is in an invalid state for the operation.
    #[cfg(unix)]
    InvalidState(SocketDescriptor),

    /// We got an `Utf8Error` while trying to convert a `CStr` into a safer string type.
    CStrConversion,

    /// We hooked a file operation on a path in mirrord's bin directory. So do the operation
    /// locally, but on the original path, not the one in mirrord's dir.
    #[cfg(target_os = "macos")]
    FileOperationInMirrordBinTempDir(*const c_char),

    /// File [`PathBuf`] should be ignored (used for tests).
    #[cfg(unix)]
    IgnoredFile(CString),

    /// Multiple file [`PathBuf`] that should be ignored.
    ///
    /// Used for functions that must apply `fs.mapping` to multiple files on bypass.
    #[cfg(unix)]
    IgnoredFiles(Option<CString>, Option<CString>),

    /// Some operations only handle absolute [`PathBuf`]s.
    #[cfg(unix)]
    RelativePath(CString),

    /// Started mirrord with [`FsModeConfig`](mirrord_config::feature::fs::mode::FsModeConfig) set
    /// to [`FsModeConfig::Read`](mirrord_config::feature::fs::FsModeConfig::Read), but
    /// operation requires more file permissions.
    ///
    /// The user will reach this case if they started mirrord with file operations as _read-only_,
    /// but tried to perform a file operation that requires _write_ permissions (for example).
    ///
    /// When this happens, the file operation will be bypassed (will be handled locally, instead of
    /// through the agent).
    #[cfg(unix)]
    ReadOnly(PathBuf),

    /// Called `write` with `write_bytes` set to [`None`].
    EmptyBuffer,

    /// Operation received [`None`] for an [`Option`] that was required to be [`Some`].
    EmptyOption,

    /// Called `getaddrinfo` with `rawish_node` being [`None`].
    NullNode,

    /// Skip patching SIP for macOS.
    #[cfg(target_os = "macos")]
    NoSipDetected(String),

    /// Tried patching SIP for a non-existing binary.
    #[cfg(target_os = "macos")]
    ExecOnNonExistingFile(String),

    /// Reached `MAX_ARGC` while running
    /// `intercept_tmp_dir`
    #[cfg(target_os = "macos")]
    TooManyArgs,

    /// Application is binding a port, while mirrord is running targetless. A targetless agent does
    /// is not exposed by a service, so bind locally.
    BindWhenTargetless,

    /// Hooked a `connect` to a target that is disabled in the configuration.
    DisabledOutgoing,

    /// Incoming traffic is disabled, bypass.
    DisabledIncoming,

    /// Hostname should be resolved locally.
    /// Currently, this is the case only when the layer operates in the `trace only` mode.
    LocalHostname,

    /// DNS query should be done locally.
    LocalDns,

    /// Operation is not implemented, but it should not be a hard error.
    ///
    /// Useful for operations that are version gated, and we want to bypass when the protocol
    /// doesn't support them.
    NotImplemented,

    /// File `open` (any `open`-ish operation) was forced to be local, instead of remote, most
    /// likely due to an operator fs policy.
    OpenLocal,

    /// Invalid argument value
    #[cfg(target_os = "macos")]
    InvalidArgValue,
}

#[cfg(unix)]
impl Bypass {
    pub fn relative_path(path: impl Into<Vec<u8>>) -> Self {
        Bypass::RelativePath(CString::new(path).expect("should be a valid C string"))
    }

    pub fn ignored_file(path: impl Into<Vec<u8>>) -> Self {
        Bypass::IgnoredFile(CString::new(path).expect("should be a valid C string"))
    }
}

/// The failure half of a [`Detour`].
///
/// Either a recoverable [`Bypass`] (the hook should defer to the original libc function) or a hard
/// [`HookError`] that gets surfaced to the user application as a libc error code.
#[derive(Debug)]
pub enum DetourError {
    /// Soft-failure that can be recovered from by calling the raw libc function.
    Bypass(Bypass),
    /// Unrecoverable error.
    Error(HookError),
}

impl From<Bypass> for DetourError {
    fn from(bypass: Bypass) -> Self {
        DetourError::Bypass(bypass)
    }
}

impl From<HookError> for DetourError {
    fn from(error: HookError) -> Self {
        DetourError::Error(error)
    }
}

/// Control flow type alias used by the layer's hooks.
///
/// - `Ok(value)` is a success, and the hook shall continue;
/// - `Err(DetourError::Bypass(_))` asks the hook to call the original libc function;
/// - `Err(DetourError::Error(_))` is a hard error surfaced to the user application.
///
/// The `?` operator works as the following:
/// - `?` on a nested [`Detour`] forwards its [`DetourError`];
/// - `?` on any error convertible into [`HookError`] maps to [`DetourError::Error`] (see the `From`
///   impls in [`crate::error`]);
/// - `?` on a [`Bypass`] maps to [`DetourError::Bypass`].
///
/// To turn a [`None`] into a bypass, use [`OptionExt::bypass`]; the bare `?` on an [`Option`] does
/// **not** produce a bypass.
pub type Detour<S = ()> = Result<S, DetourError>;

/// Extension methods on [`Detour`] that need to tell [`DetourError::Bypass`] apart from
/// [`DetourError::Error`].
pub trait DetourExt<S> {
    /// Calls `op` only on [`DetourError::Error`], leaving `Ok` and [`DetourError::Bypass`]
    /// untouched.
    ///
    /// Named `on_error` (not `or_else`) so it does not silently shadow [`Result::or_else`], which
    /// would fire on a [`DetourError::Bypass`] too.
    fn on_error<O: FnOnce(HookError) -> Detour<S>>(self, op: O) -> Detour<S>;

    /// Calls `op` only on [`DetourError::Bypass`], leaving `Ok` and [`DetourError::Error`]
    /// untouched.
    fn or_bypass<O: FnOnce(Bypass) -> Detour<S>>(self, op: O) -> Detour<S>;

    /// Helper function for returning a detour return value from a hook.
    ///
    /// - `Ok` -> return the contained value;
    /// - `Bypass` -> call `op` and return its value;
    /// - `Error` -> convert to a libc value and return it.
    fn unwrap_or_bypass_with<F: FnOnce(Bypass) -> S>(self, op: F) -> S
    where
        S: From<HookError>;

    /// Helper function for returning a detour return value from a hook.
    ///
    /// - `Ok` -> return the contained value;
    /// - `Bypass` -> return the provided `value`;
    /// - `Error` -> convert to a libc value and return it.
    fn unwrap_or_bypass(self, value: S) -> S
    where
        S: From<HookError>;
}

impl<S> DetourExt<S> for Detour<S> {
    #[inline]
    fn on_error<O: FnOnce(HookError) -> Detour<S>>(self, op: O) -> Detour<S> {
        match self {
            Ok(s) => Ok(s),
            Err(DetourError::Bypass(b)) => Err(DetourError::Bypass(b)),
            Err(DetourError::Error(e)) => op(e),
        }
    }

    #[inline]
    fn or_bypass<O: FnOnce(Bypass) -> Detour<S>>(self, op: O) -> Detour<S> {
        match self {
            Ok(s) => Ok(s),
            Err(DetourError::Bypass(b)) => op(b),
            Err(DetourError::Error(e)) => Err(DetourError::Error(e)),
        }
    }

    fn unwrap_or_bypass_with<F: FnOnce(Bypass) -> S>(self, op: F) -> S
    where
        S: From<HookError>,
    {
        match self {
            Ok(s) => s,
            Err(DetourError::Bypass(b)) => op(b),
            Err(DetourError::Error(e)) => e.into(),
        }
    }

    fn unwrap_or_bypass(self, value: S) -> S
    where
        S: From<HookError>,
    {
        match self {
            Ok(s) => s,
            Err(DetourError::Bypass(_)) => value,
            Err(DetourError::Error(e)) => e.into(),
        }
    }
}

/// Extends `Option<T>` with the `Option::bypass` function.
pub trait OptionExt {
    /// Inner `T` of the `Option<T>`.
    type Opt;

    /// Converts `Option<T>` into `Detour<T>`, mapping:
    ///
    /// - `Some` => `Detour::Success`;
    /// - `None` => `Detour::Bypass`.
    fn bypass(self, value: Bypass) -> Detour<Self::Opt>;
}

impl<T> OptionExt for Option<T> {
    type Opt = T;

    fn bypass(self, value: Bypass) -> Detour<T> {
        match self {
            Some(v) => Ok(v),
            None => Err(DetourError::Bypass(value)),
        }
    }
}

/// Extends [`OnceLock`] with a helper function to initialize it with a [`Detour`].
pub trait OnceLockExt<T> {
    /// Initializes a [`OnceLock`] with a [`Detour`] (similar to [`OnceLock::get_or_try_init`]).
    fn get_or_detour_init<F>(&self, f: F) -> Detour<&T>
    where
        F: FnOnce() -> Detour<T>;
}

impl<T> OnceLockExt<T> for OnceLock<T> {
    fn get_or_detour_init<F>(&self, f: F) -> Detour<&T>
    where
        F: FnOnce() -> Detour<T>,
    {
        if let Some(value) = self.get() {
            Ok(value)
        } else {
            let value = f()?;

            Ok(self.get_or_init(|| value))
        }
    }
}

#[cfg(windows)]
/// Encodes Windows ABI-specific return/error behavior for a detoured hook.
///
/// Windows APIs with similar C signatures can still require different error semantics.
/// For example:
/// - `SOCKET`-returning creation calls use `INVALID_SOCKET` plus `WSASetLastError`.
/// - many WinSock operations use `SOCKET_ERROR` plus `WSASetLastError`.
/// - `getaddrinfo` returns its error code directly instead of setting WinSock last error.
///
/// Marker types implementing this trait let hooks state that contract explicitly and avoid
/// accidental conversions from a generic `HookError -> ReturnType`.
pub trait WindowsDetourReturn<S> {
    type Return;

    fn error_return(errno: i32) -> Self::Return;

    fn set_wsa_last_error() -> bool {
        true
    }

    fn from_success(value: S) -> Self::Return
    where
        S: Into<Self::Return>,
    {
        value.into()
    }

    fn from_error(error: HookError) -> Self::Return {
        let errno = get_platform_errno(error) as i32;
        if Self::set_wsa_last_error() {
            unsafe { WSASetLastError(errno) };
        }

        Self::error_return(errno)
    }
}

#[cfg(windows)]
macro_rules! impl_windows_detour_return {
    ($name:ident, $ret:ty, $body:expr) => {
        pub struct $name;

        impl<S> WindowsDetourReturn<S> for $name
        where
            S: Into<$ret>,
        {
            type Return = $ret;

            fn error_return(errno: i32) -> Self::Return {
                ($body)(errno)
            }
        }
    };
}

#[cfg(windows)]
impl_windows_detour_return!(WinSocket, SOCKET, |_| INVALID_SOCKET);

#[cfg(windows)]
impl_windows_detour_return!(WinGetAddrInfoInt, INT, |errno: i32| errno as INT);

/// Windows-only [`Detour`] helper for turning a hook result into an ABI-specific return value.
///
/// Lives on its own trait (rather than as an inherent method) because [`Detour`] is a [`Result`]
/// alias, which cannot carry inherent `impl`s.
#[cfg(windows)]
pub trait DetourWindowsExt<S> {
    /// Turns the hook result into a return value using `K`'s Windows return ABI semantics.
    ///
    /// `K` controls how `Ok` and [`DetourError::Error`] are converted into return values and how
    /// errors are surfaced to the caller. Use this for Windows hooks instead of relying on generic
    /// `From<HookError>` conversions.
    fn unwrap_or_bypass_windows_as<K, F>(self, op: F) -> K::Return
    where
        K: WindowsDetourReturn<S>,
        S: Into<K::Return>,
        F: FnOnce(Bypass) -> K::Return;
}

#[cfg(windows)]
impl<S> DetourWindowsExt<S> for Detour<S> {
    fn unwrap_or_bypass_windows_as<K, F>(self, op: F) -> K::Return
    where
        K: WindowsDetourReturn<S>,
        S: Into<K::Return>,
        F: FnOnce(Bypass) -> K::Return,
    {
        match self {
            Ok(value) => K::from_success(value),
            Err(DetourError::Bypass(reason)) => op(reason),
            Err(DetourError::Error(error)) => K::from_error(error),
        }
    }
}
