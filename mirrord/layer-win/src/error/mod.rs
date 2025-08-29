//! `layer-win` errors.

pub(crate) mod windows;

use std::{env::VarError, net::AddrParseError};

use thiserror::Error;

use crate::error::windows::WindowsError;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Failed applying hook for function {0}, dll {1}")]
    FailedApplyingAPIHook(String, String),

    #[error("Environment variable for intproxy address is missing: {0}")]
    MissingEnvIntProxyAddr(VarError),
    #[error("Intproxy address malformed: {0}")]
    MalformedIntProxyAddr(AddrParseError),

    #[error("Failed allocating console: {0}")]
    FailedAllocatingConsole(WindowsError),
    #[error("Failed redirecting Std handles: {0}")]
    FailedRedirectingStdHandles(WindowsError),
}

#[allow(unused)]
pub type Result<T> = std::result::Result<T, Error>;
