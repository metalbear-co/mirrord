use thiserror::Error;

#[derive(Debug, Error)]
pub enum RemoteBootstrapError {
    #[error(transparent)]
    IO(#[from] std::io::Error),
    #[error(transparent)]
    Null(#[from] std::ffi::NulError),
    #[error("Failed locating bootstrap location: {0}")]
    DlAddr(String),
}

pub(crate) type Result<T> = std::result::Result<T, RemoteBootstrapError>;
