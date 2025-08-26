//! `layer-win` errors.

use std::{
    env::VarError,
    net::{AddrParseError, SocketAddr},
};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Failed applying hook for function {0}, dll {1}")]
    FailedApplyingAPIHook(String, String),

    #[error("Environment variable for intproxy address is missing: {0}")]
    MissingEnvIntProxyAddr(VarError),
    #[error("Intproxy address malformed: {0}")]
    MalformedIntProxyAddr(AddrParseError),
}

#[allow(unused)]
pub type Result<T> = std::result::Result<T, Error>;
