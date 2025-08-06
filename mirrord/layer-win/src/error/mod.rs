//! `layer-win` errors.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Failed applying hook for function {0}, dll {1}")]
    FailedApplyingAPIHook(String, String),
}

pub type Result<T> = std::result::Result<T, Error>;
