use std::env::VarError;

use mirrord_remote_layer_protocol::error::RemoteLayerProtocolError;
use thiserror::Error;

#[derive(Debug, Error)]
pub(crate) enum RemoteLayerError {
    #[error("io error: {0}")]
    IO(#[from] std::io::Error),
    #[error("Env var error: {0}")]
    EnvVar(#[from] VarError),
    #[error("RemoteLayer protocol error: {0}")]
    RemoteLayerProtocol(#[from] RemoteLayerProtocolError),
    #[error("Handoff socket codec error: {0}")]
    Codec(#[from] mirrord_intproxy_protocol::codec::CodecError),
    #[error("Handoff socket sendmsg error: {0}")]
    NixErrno(#[from] nix::errno::Errno),
}
pub(crate) type Result<T> = std::result::Result<T, RemoteLayerError>;
