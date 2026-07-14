use thiserror::Error;

#[derive(Debug, Error)]
pub enum RemoteBootstrapError {
    #[error("IO Error: {0}")]
    IO(#[from] std::io::Error),
    #[error("Null error {0}")]
    Null(#[from] std::ffi::NulError),
}
pub(crate) type Result<T> = std::result::Result<T, RemoteBootstrapError>;
