//! The layer uses features from this module to check if it should bypass one of its hooks, and call
//! the original [`libc`] function.
//!
//! Here we also have the convenient [`Detour`], that is used by the hooks to either return a
//! [`Result`]-like value, or the special [`Bypass`] case, which makes the _detour_ function call
//! the original [`libc`] equivalent, stored in a [`HookFn`].

use core::{
    convert,
    ops::{FromResidual, Residual, Try},
};
use std::{cell::RefCell, ops::Deref, os::unix::prelude::*, path::PathBuf, sync::OnceLock};

use crate::error::HookError;

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
    static DETOUR_BYPASS: RefCell<bool> = RefCell::new(false)
);

/// Sets [`DETOUR_BYPASS`] to `true`, bypassing the layer's detours.
///
/// Prefer using `DetourGuard::new` instead.
pub(super) fn detour_bypass_on() {
    DETOUR_BYPASS.with(|enabled| *enabled.borrow_mut() = true);
}

/// Sets [`DETOUR_BYPASS`] to `false`.
///
/// Prefer relying on the [`Drop`] implementation of [`DetourGuard`] instead.
pub(super) fn detour_bypass_off() {
    DETOUR_BYPASS.with(|enabled| *enabled.borrow_mut() = false);
}

/// Handler for the layer's [`DETOUR_BYPASS`].
///
/// Sets [`DETOUR_BYPASS`] on creation, and turns it off on [`Drop`].
///
/// ## Warning
///
/// You should always use `DetourGuard::new`, if you construct this in any other way, it's
/// not going to guard anything.
pub(crate) struct DetourGuard;

impl DetourGuard {
    /// Create a new DetourGuard if it's not already enabled.
    pub(crate) fn new() -> Option<Self> {
        DETOUR_BYPASS.with(|enabled| {
            if *enabled.borrow() {
                None
            } else {
                *enabled.borrow_mut() = true;
                Some(Self)
            }
        })
    }
}

impl Drop for DetourGuard {
    fn drop(&mut self) {
        detour_bypass_off();
    }
}

/// Wrapper around [`OnceLock`](std::sync::OnceLock), mainly used for the [`Deref`] implementation
/// to simplify calls to the original functions as `FN_ORIGINAL()`, instead of
/// `FN_ORIGINAL.get().unwrap()`.
#[derive(Debug)]
pub(crate) struct HookFn<T>(OnceLock<T>);

impl<T> Deref for HookFn<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.0.get().unwrap()
    }
}

impl<T> const Default for HookFn<T> {
    fn default() -> Self {
        Self(OnceLock::new())
    }
}

impl<T> HookFn<T> {
    /// Helper function to set the inner [`OnceLock`](std::sync::OnceLock) `T` of `self`.
    pub(crate) fn set(&self, value: T) -> Result<(), T> {
        self.0.set(value)
    }
}

/// Soft-errors that can be recovered from by calling the raw FFI function.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Bypass {
    /// We're dealing with a socket port value that should be ignored.
    Port(u16),

    /// The socket type does not match one of our handled
    /// [`SocketKind`](crate::socket::SocketKind)s.
    Type(i32),

    /// Either an invalid socket domain, or one that we don't handle.
    Domain(i32),

    /// Unix socket to address that was not configured to be connected remotely.
    UnixSocket(Option<String>),

    /// We could not find this [`RawFd`] in neither [`OPEN_FILES`](crate::file::OPEN_FILES), nor
    /// [`SOCKETS`](crate::socket::SOCKETS).
    LocalFdNotFound(RawFd),

    /// Similar to `LocalFdNotFound`, but for [`OPEN_DIRS`](crate::file::OPEN_DIRS).
    LocalDirStreamNotFound(usize),

    /// A conversion from [`SockAddr`](socket2::sockaddr::SockAddr) to
    /// [`SocketAddr`](std::net::SocketAddr) failed.
    AddressConversion,

    /// The socket [`RawFd`] is in an invalid state for the operation.
    InvalidState(RawFd),

    /// We got an `Utf8Error` while trying to convert a `CStr` into a safer string type.
    CStrConversion,

    /// File [`PathBuf`] should be ignored (used for tests).
    IgnoredFile(PathBuf),

    /// Some operations only handle absolute [`PathBuf`]s.
    RelativePath(PathBuf),

    /// Started mirrord with [`FsModeConfig`](mirrord_config::fs::mode::FsModeConfig) set to
    /// [`FsModeConfig::Read`](mirrord_config::fs::mode::FsModeConfig::Read), but operation
    /// requires more file permissions.
    ///
    /// The user will reach this case if they started mirrord with file operations as _read-only_,
    /// but tried to perform a file operation that requires _write_ permissions (for example).
    ///
    /// When this happens, the file operation will be bypassed (will be handled locally, instead of
    /// through the agent).
    ReadOnly(PathBuf),

    /// Called [`write`](crate::file::ops::write) with `write_bytes` set to [`None`].
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

    /// Socket is connecting to localhots and we're asked to ignore it.
    IgnoreLocalhost(u16),

    /// Hooked connect from a bound mirror socket.
    MirrorConnect,

    /// Hooked a `connect` to a target that is disabled in the configuration.
    DisabledOutgoing,
}

/// [`ControlFlow`](std::ops::ControlFlow)-like enum to be used by hooks.
///
/// Conversion from `Result`:
/// - `Result::Ok` -> `Detour::Success`
/// - `Result::Err` -> `Detour::Error`
///
/// Conversion from `Option`:
/// - `Option::Some` -> `Detour::Success`
/// - `Option::None` -> `Detour::Bypass`
#[derive(Debug)]
pub(crate) enum Detour<S = ()> {
    /// Equivalent to `Result::Ok`
    Success(S),
    /// Useful for operations with parameters that are ignored by `mirrord`, or for soft-failures
    /// (errors that can be recovered from in the hook FFI).
    Bypass(Bypass),
    /// Equivalent to `Result::Err`
    Error(HookError),
}

impl<S> Try for Detour<S> {
    type Output = S;

    type Residual = Detour<convert::Infallible>;

    fn from_output(output: Self::Output) -> Self {
        Detour::Success(output)
    }

    fn branch(self) -> std::ops::ControlFlow<Self::Residual, Self::Output> {
        match self {
            Detour::Success(s) => core::ops::ControlFlow::Continue(s),
            Detour::Bypass(b) => core::ops::ControlFlow::Break(Detour::Bypass(b)),
            Detour::Error(e) => core::ops::ControlFlow::Break(Detour::Error(e)),
        }
    }
}

impl<S> FromResidual<Detour<convert::Infallible>> for Detour<S> {
    fn from_residual(residual: Detour<convert::Infallible>) -> Self {
        match residual {
            Detour::Success(_) => unreachable!(),
            Detour::Bypass(b) => Detour::Bypass(b),
            Detour::Error(e) => Detour::Error(e),
        }
    }
}

impl<S, E> FromResidual<Result<convert::Infallible, E>> for Detour<S>
where
    E: Into<HookError>,
{
    fn from_residual(residual: Result<convert::Infallible, E>) -> Self {
        match residual {
            Ok(_) => unreachable!(),
            Err(e) => Detour::Error(e.into()),
        }
    }
}

impl<S> FromResidual<Result<convert::Infallible, Bypass>> for Detour<S> {
    fn from_residual(residual: Result<convert::Infallible, Bypass>) -> Self {
        match residual {
            Ok(_) => unreachable!(),
            Err(e) => Detour::Bypass(e),
        }
    }
}

impl<S> FromResidual<Option<convert::Infallible>> for Detour<S> {
    fn from_residual(residual: Option<convert::Infallible>) -> Self {
        match residual {
            Some(_) => unreachable!(),
            None => Detour::Bypass(Bypass::EmptyOption),
        }
    }
}

impl<S> Residual<S> for Detour<convert::Infallible> {
    type TryType = Detour<S>;
}

impl<S> Detour<S> {
    /// Calls `op` if the result is `Success`, otherwise returns the `Bypass` or `Error` value of
    /// self.
    ///
    /// This function can be used for control flow based on `Detour` values.
    pub(crate) fn and_then<U, F: FnOnce(S) -> Detour<U>>(self, op: F) -> Detour<U> {
        match self {
            Detour::Success(s) => op(s),
            Detour::Bypass(b) => Detour::Bypass(b),
            Detour::Error(e) => Detour::Error(e),
        }
    }

    /// Maps a `Detour<S>` to `Detour<U>` by applying a function to a contained `Success` value,
    /// leaving a `Bypass` or `Error` value untouched.
    ///
    /// This function can be used to compose the results of two functions.
    pub(crate) fn map<U, F: FnOnce(S) -> U>(self, op: F) -> Detour<U> {
        match self {
            Detour::Success(s) => Detour::Success(op(s)),
            Detour::Bypass(b) => Detour::Bypass(b),
            Detour::Error(e) => Detour::Error(e),
        }
    }

    /// Return the contained `Success` value or a provided default if `Bypass` or `Error`.
    ///
    /// To be used in hooks that are deemed non-essential, and the run should continue even if they
    /// fail.
    ///
    /// Currently defined only on macos because it is only used in macos-only code.
    /// Remove the cfg attribute to enable using in other code.
    #[cfg(target_os = "macos")]
    pub(crate) fn unwrap_or(self, default: S) -> S {
        match self {
            Detour::Success(s) => s,
            _ => default,
        }
    }
}

impl<S> Detour<S>
where
    S: From<HookError>,
{
    /// Helper function for returning a detour return value from a hook.
    ///
    /// - `Success` -> Return the contained value.
    /// - `Bypass` -> Call the bypass and return its value.
    /// - `Error` -> Convert to libc value and return it.
    pub(crate) fn unwrap_or_bypass_with<F: FnOnce(Bypass) -> S>(self, op: F) -> S {
        match self {
            Detour::Success(s) => s,
            Detour::Bypass(b) => op(b),
            Detour::Error(e) => e.into(),
        }
    }

    /// Helper function for returning a detour return value from a hook.
    ///
    /// `Success` -> Return the contained value.
    /// `Bypass` -> Return provided value.
    /// `Error` -> Convert to libc value and return it.
    pub(crate) fn unwrap_or_bypass(self, value: S) -> S {
        match self {
            Detour::Success(s) => s,
            Detour::Bypass(_) => value,
            Detour::Error(e) => e.into(),
        }
    }
}

/// Extends `Option<T>` with the `Option::bypass` function.
pub(crate) trait OptionExt {
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
            Some(v) => Detour::Success(v),
            None => Detour::Bypass(value),
        }
    }
}

/// Extends [`OnceLock`] with a helper function to initialize it with a [`Detour`].
pub(crate) trait OnceLockExt<T> {
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
            Detour::Success(value)
        } else {
            let value = f()?;

            Detour::Success(self.get_or_init(|| value))
        }
    }
}
