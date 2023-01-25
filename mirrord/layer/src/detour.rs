use core::{
    convert,
    ops::{FromResidual, Residual, Try},
};
use std::{cell::RefCell, ops::Deref, os::unix::prelude::*, path::PathBuf};

use crate::error::HookError;

thread_local!(
    /// Holds the thread-local state for bypassing the layer's detour functions.
    ///
    /// ## Warning
    ///
    /// Do **NOT** use this directly, instead use [`DetourGuard::new`](link) if you need to
    /// create a bypass inside a function (like we have in
    /// [`TcpHandler::create_local_stream`](crate::tcp::TcpHandler::create_local_stream)).
    ///
    /// Or rely on the [`hook_guard_fn`](mirrord_layer_macro::hook_guard_fn) macro.
    ///
    /// ## Details
    ///
    /// Some of the layer functions will interact with [`libc`] functions that we are hooking into,
    /// thus we could end up _stealing_ an actual call that we want to make. An example of this
    /// would be if we wanted to open a file locally, the layer's [`open_detour`](link) intercepts
    /// the [`libc::open`] call, and we get a remote file (if it exists), instead of the local
    /// we wanted.
    ///
    /// We set this to `true` whenever an operation may require calling other [`libc`] functions,
    /// and back to `false` after it's done.
    pub(crate) static DETOUR_BYPASS: RefCell<bool> = RefCell::new(false)
);

/// Sets [`DETOUR_BYPASS`] to `true`, bypassing the layer's detours.
///
/// Prefer using [`DetourGuard::new`](link) instead.
pub(crate) fn detour_bypass_on() {
    DETOUR_BYPASS.with(|enabled| *enabled.borrow_mut() = true);
}

/// Sets [`DETOUR_BYPASS`] to `false`.
///
/// Prefer relying on the [`Drop`] implementation of [`DetourGuard`] instead.
pub(crate) fn detour_bypass_off() {
    DETOUR_BYPASS.with(|enabled| *enabled.borrow_mut() = false);
}

/// Handler for the layer's [`DETOUR_BYPASS`].
///
/// Sets [`DETOUR_BYPASS`] on creation, and turns it off on [`Drop`].
///
/// ## Warning
///
/// You should always use [`DetourGuard::new`](link), if you construct this in any other way, it's
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
pub(crate) struct HookFn<T>(std::sync::OnceLock<T>);

impl<T> Deref for HookFn<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.0.get().unwrap()
    }
}

impl<T> const Default for HookFn<T> {
    fn default() -> Self {
        Self(std::sync::OnceLock::new())
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
    Port(u16),
    Type(i32),
    Domain(i32),
    LocalFdNotFound(RawFd),
    LocalDirStreamNotFound(usize),
    AddressConversion,
    InvalidState(RawFd),
    CStrConversion,
    IgnoredFile(PathBuf),
    RelativePath(PathBuf),
    ReadOnly(PathBuf),
    EmptyBuffer,
    EmptyOption,
    NullNode,
    #[cfg(target_os = "macos")]
    NoSipDetected(String),
    #[cfg(target_os = "macos")]
    ExecOnNonExistingFile(String),
    #[cfg(target_os = "macos")]
    NoTempDirInArgv,
    #[cfg(target_os = "macos")]
    TempDirEnvVarNotSet,
    #[cfg(target_os = "macos")]
    TooManyArgs,
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
    // Useful for operations with parameters that are ignored by `mirrord`, or for soft-failures
    // (errors that can be recovered from in the hook FFI).
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
    pub(crate) fn and_then<U, F: FnOnce(S) -> Detour<U>>(self, op: F) -> Detour<U> {
        match self {
            Detour::Success(s) => op(s),
            Detour::Bypass(b) => Detour::Bypass(b),
            Detour::Error(e) => Detour::Error(e),
        }
    }

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
    /// Currently defined only on macos because it is only used in macos-only code.
    /// Remove the cfg attribute to enable using in other code.
    #[cfg(target_os = "macos")]
    pub(crate) fn unwrap_or(self, default: S) -> S {
        match self {
            Detour::Success(s) => s,
            Detour::Bypass(_) | Detour::Error(_) => default,
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
    type Opt;

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
