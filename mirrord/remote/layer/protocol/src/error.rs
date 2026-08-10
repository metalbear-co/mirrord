use mirrord_intproxy_protocol::codec::CodecError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RemoteLayerProtocolError {
    #[error("io error: {0}")]
    IO(#[from] std::io::Error),
    #[error("Env var error: {0}")]
    EnvVar(#[from] std::env::VarError),
    #[error("Handoff Socket is missing: {0}")]
    HandoffSocketMissing(String),
    #[error("nix error: {0}")]
    Errno(#[from] nix::errno::Errno),
    #[error("codec error: {0}")]
    Codec(#[from] CodecError),
}
pub type Result<T> = std::result::Result<T, RemoteLayerProtocolError>;
