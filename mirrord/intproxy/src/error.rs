use std::io;

use mirrord_protocol::DaemonMessage;
use thiserror::Error;

use crate::{
    agent_conn::{AgentClosedConnection, AgentConnectionError},
    codec::CodecError,
    ping_pong::PingPongError,
    proxies::outgoing::OutgoingProxyError,
    request_queue::RequestQueueEmpty,
    session::MainTaskId,
};

#[derive(Error, Debug)]
pub enum IntProxyError {
    #[error("waiting for the first layer connection timed out")]
    FirstConnectionTimeout,
    #[error("accepting layer connection failed: {0}")]
    AcceptFailed(io::Error),
    #[error("connecting with agent failed: {0}")]
    AgentConnectFailed(#[from] AgentConnectionError),
    #[error("agent closed connection with error: {0}")]
    AgentFailed(String),
    #[error("agent sent unexpected message: {0:?}")]
    UnexpectedAgentMessage(DaemonMessage),

    #[error("background task {0} exited unexpectedly")]
    TaskExit(MainTaskId),
    #[error("background task {0} panicked")]
    TaskPanic(MainTaskId),

    #[error("{0}")]
    AgentConnectionError(#[from] AgentClosedConnection),
    #[error("ping pong failed: {0}")]
    PingPong(#[from] PingPongError),
    #[error("layer connection failed: {0}")]
    LayerConnectionError(#[from] CodecError),
    #[error("simple proxy failed: {0}")]
    SimpleProxy(#[from] RequestQueueEmpty),
    #[error("outgoing proxy failed: {0}")]
    OutgoingProxy(#[from] OutgoingProxyError),
}

pub type Result<T> = core::result::Result<T, IntProxyError>;
