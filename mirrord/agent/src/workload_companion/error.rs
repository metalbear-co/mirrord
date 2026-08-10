use mirrord_intproxy_protocol::codec::CodecError;
use thiserror::Error;

/// Errors that can occur while negotiating a connection handoff with the remote layer over the
/// [`CONNECTION_HANDOFF_SOCKET_ENV`](mirrord_remote_layer_protocol::CONNECTION_HANDOFF_SOCKET_ENV)
/// socket.
///
/// The handoff wire types live in `mirrord-remote-layer-protocol`, but the socket, fd-passing and
/// framing logic is entirely the workload-companion's own, so its failures belong here rather than
/// in that shared, data-only crate.
#[derive(Debug, Error)]
pub enum RemoteIncomingHandoffError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Codec(#[from] CodecError),
}

pub(crate) type Result<T> = std::result::Result<T, RemoteIncomingHandoffError>;
