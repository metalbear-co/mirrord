use std::io;

use thiserror::Error;

use crate::{
    agent_conn::AgentCommunicationError, codec::CodecError, layer_conn::LayerCommunicationError,
    ping_pong::PingPongError, request_queue::RequestQueueEmpty,
};

#[derive(Error, Debug)]
pub enum IntProxyError {
    #[error("waiting for the first layer connection timed out")]
    FirstConnectionTimeout,
    #[error("accepting layer connection failed: {0}")]
    AcceptFailed(io::Error),
    #[error("communication with agent failed: {0}")]
    AgentCommunicationError(#[from] AgentCommunicationError),
    #[error("communication with layer failed: {0}")]
    LayerCommunicationError(#[from] LayerCommunicationError),
    #[error("agent closed connection: {0}")]
    AgentClosedConnection(String),
    #[error("ping pong failed: {0}")]
    PingPong(#[from] PingPongError),
    #[error("layer connector failed: {0}")]
    LayerConnector(#[from] CodecError),
    #[error("simple proxy failed: {0}")]
    SimpleProxy(#[from] RequestQueueEmpty),
    // #[error("outgoing proxy failed: {0}")]
    // OutgoingProxy(#[from] OutgoingProxyError),
}

pub type Result<T> = core::result::Result<T, IntProxyError>;
