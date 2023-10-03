use std::io;

use mirrord_protocol::ResponseError;
use thiserror::Error;

use crate::{
    agent_conn::AgentCommunicationFailed, codec::CodecError, ping_pong::PingPongError,
    proxies::outgoing::proxy::OutgoingProxyError, request_queue::RequestQueueEmpty,
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
    #[error("received error from agent: {0}")]
    AgentError(#[from] ResponseError),
    #[error("connection id {0} not found")]
    NoConnectionId(u64),
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("outgoing interceptor task failed")]
    OutgoingInterceptorFailed,
    #[error("ping pong error: {0}")]
    PingPong(#[from] PingPongError),
    #[error("incoming interceptor task failed")]
    IncomingInterceptorFailed,

    #[error("layer connector failed: {0}")]
    LayerConnectorError(#[from] CodecError),
    #[error("simple proxy failed: {0}")]
    SimpleProxyError(#[from] RequestQueueEmpty),
    #[error("outgoing proxy failed: {0}")]
    OutgoingProxyError(#[from] OutgoingProxyError),

    #[error("{0}")]
    ComponentError(#[from] ComponentError<&'static str, Box<IntProxyError>>),
}

pub type Result<T> = core::result::Result<T, IntProxyError>;
