use mirrord_remote_layer_protocol::error::RemoteLayerProtocolError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RemoteBootstrapError {
    #[error("Layer Protocol Error: {0}")]
    Protocol(#[from] RemoteLayerProtocolError),
    #[error(transparent)]
    IO(#[from] std::io::Error),
    #[error(transparent)]
    Null(#[from] std::ffi::NulError),
    #[error("Failed locating bootstrap location: {0}")]
    DlAddr(String),
    #[error("Error loading remote-layer: {0}")]
    LayerLoad(String),
    #[error(
        "Waiting for agent workload-companion timed-out after {1} seconds for creation of socket file {0}"
    )]
    AgentTimeout(std::path::PathBuf, u64),
}

pub(crate) type Result<T> = std::result::Result<T, RemoteBootstrapError>;
