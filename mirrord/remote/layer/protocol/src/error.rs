use mirrord_intproxy_protocol::codec::CodecError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RemoteLayerProtocolError {
    #[error("io error: {0}")]
    IO(#[from] std::io::Error),
    #[error("nix error: {0}")]
    Nix(#[from] nix::Error),
    #[error("codec error: {0}")]
    Codec(#[from] CodecError),
}
