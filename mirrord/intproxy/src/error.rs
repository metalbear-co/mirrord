use std::io;

use mirrord_protocol::ResponseError;
use thiserror::Error;

use crate::{agent_conn::AgentCommunicationFailed, layer_conn::LayerCommunicationFailed};

#[derive(Error, Debug)]
pub enum IntProxyError {
    #[error("waiting for the first layer connection timed out")]
    FirstConnectionTimeout,
    #[error("accepting layer connection failed: {0}")]
    AcceptFailed(io::Error),
    #[error("communication with agent failed: {0}")]
    AgentCommunicationFailed(#[from] AgentCommunicationFailed),
    #[error("communication with layer failed: {0}")]
    LayerCommunicationFailed(#[from] LayerCommunicationFailed),
    #[error("request queue is empty")]
    RequestQueueEmpty,
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
    #[error("sending datagrams over unix sockets is not supported")]
    DatagramOverUnix,
}

pub type Result<T> = core::result::Result<T, IntProxyError>;
