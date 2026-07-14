use mirrord_remote_layer_protocol::error::RemoteLayerProtocolError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RemoteBootstrapError {
    #[error("Layer Protocol Error: {0}")]
    Protocol(#[from] RemoteLayerProtocolError),
    #[error("IO Error: {0}")]
    IO(#[from] std::io::Error),
    #[error("Null error {0}")]
    Null(#[from] std::ffi::NulError),
    #[error("Error loading remote-layer: {0}")]
    LayerLoad(String),
    #[error(
        "Waiting for agent sidecar timed-out after {1} seconds for creation of socket file {0}"
    )]
    AgentTimeout(std::path::PathBuf, u64),
}
pub(crate) type Result<T> = std::result::Result<T, RemoteBootstrapError>;
