use std::io;

use thiserror::Error;

use crate::{
    agent_conn::AgentCommunicationFailed, codec::CodecError, ping_pong::PingPongError,
    proxies::outgoing::OutgoingProxyError, request_queue::RequestQueueEmpty,
    system::ComponentError,
};

#[derive(Error, Debug)]
pub enum IntProxyError {
    #[error("waiting for the first layer connection timed out")]
    FirstConnectionTimeout,
    #[error("accepting layer connection failed: {0}")]
    AcceptFailed(io::Error),
    #[error("communication with agent failed: {0}")]
    AgentCommunicationFailed(#[from] AgentCommunicationFailed),
    #[error("agent closed connection: {0}")]
    AgentClosedConnection(String),
    #[error("ping pong failed: {0}")]
    PingPong(#[from] PingPongError),
    #[error("layer connector failed: {0}")]
    LayerConnector(#[from] CodecError),
    #[error("simple proxy failed: {0}")]
    SimpleProxy(#[from] RequestQueueEmpty),
    #[error("outgoing proxy failed: {0}")]
    OutgoingProxy(#[from] OutgoingProxyError),
    #[error("{0}")]
    ComponentError(#[from] ComponentError<&'static str, Box<IntProxyError>>),
}

pub type Result<T> = core::result::Result<T, IntProxyError>;
