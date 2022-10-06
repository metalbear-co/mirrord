use core::{
    convert,
    ops::{FromResidual, Residual, Try},
};
use std::{cell::RefCell, ops::Deref, os::unix::prelude::*};

use tracing::warn;

use crate::error::HookError;

thread_local!(pub(crate) static DETOUR_BYPASS: RefCell<bool> = RefCell::new(false));

pub(crate) fn detour_bypass_on() {
    DETOUR_BYPASS.with(|enabled| *enabled.borrow_mut() = true);
}

pub(crate) fn detour_bypass_off() {
    DETOUR_BYPASS.with(|enabled| *enabled.borrow_mut() = false);
}

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

/// Wrapper around `std::sync::OnceLock`, mainly used for the `Deref` implementation to simplify
/// calls to the original functions as `FN_ORIGINAL()`, instead of `FN_ORIGINAL.get().unwrap()`.
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
    pub(crate) fn set(&self, value: T) -> Result<(), T> {
        self.0.set(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Bypass {
    Port(u16),
    Type(i32),
    Domain(i32),
    LocalFdNotFound(RawFd),
    AddressConversion,
    InvalidState(RawFd),
    CStrConversion,
}

// TODO(alex) [high] 2022-10-04: No need for so much generics, the enum should be just what it needs
// to be.
pub(crate) enum Detour<S = ()> {
    Success(S),
    Bypass(Bypass),
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

impl<S> Residual<S> for Detour<convert::Infallible> {
    type TryType = Detour<S>;
}

impl<S> Detour<S> {
    pub(crate) fn bypass<U>(self, value: U) -> Result<U, HookError>
    where
        U: From<S>,
    {
        self.bypass_with(|_| value)
    }

    pub(crate) fn bypass_with<U, F: FnOnce(Bypass) -> U>(self, op: F) -> Result<U, HookError>
    where
        U: From<S>,
    {
        match self {
            Detour::Success(s) => Ok(s.into()),
            Detour::Bypass(b) => {
                warn!("Bypassing operation due to {:#?}", b);
                Ok(op(b))
            }
            Detour::Error(e) => Err(e),
        }
    }

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
}

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
