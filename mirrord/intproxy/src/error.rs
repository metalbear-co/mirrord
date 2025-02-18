use std::io;

use mirrord_intproxy_protocol::{codec::CodecError, LayerToProxyMessage};
use mirrord_protocol::{DaemonMessage, ErrorKindInternal, RemoteIOError, ResponseError};
use thiserror::Error;

use crate::{
    agent_conn::{AgentChannelError, AgentConnectionError},
    layer_initializer::LayerInitializerError,
    ping_pong::PingPongError,
    proxies::{
        files::FilesProxyError, incoming::IncomingProxyError, outgoing::OutgoingProxyError,
        simple::SimpleProxyError,
    },
    MainTaskId,
};

#[derive(Error, Debug)]
#[error("agent sent an unexpected message: {0:?}")]
pub struct UnexpectedAgentMessage(pub DaemonMessage);

#[derive(Error, Debug)]
pub enum IntProxyError {
    #[error("waiting for the first layer connection timed out")]
    ConnectionAcceptTimeout,
    #[error("accepting layer connection failed: {0}")]
    ConnectionAccept(io::Error),
    #[error("layer sent unexpected message: {0:?}")]
    UnexpectedLayerMessage(LayerToProxyMessage),

    #[error("connecting with agent failed: {0}")]
    AgentConnection(#[from] AgentConnectionError),
    #[error("agent closed connection with error: {0}")]
    AgentFailed(String),
    #[error(transparent)]
    UnexpectedAgentMessage(#[from] UnexpectedAgentMessage),

    #[error("background task {0} exited unexpectedly")]
    TaskExit(MainTaskId),
    #[error("background task {0} panicked")]
    TaskPanic(MainTaskId),

    #[error("{0}")]
    AgentChannel(#[from] AgentChannelError),
    #[error("layer initializer failed: {0}")]
    LayerInitializer(#[from] LayerInitializerError),
    #[error("ping pong failed: {0}")]
    PingPong(#[from] PingPongError),
    #[error("layer connection failed: {0}")]
    LayerConnection(#[from] CodecError),
    #[error("simple proxy failed: {0}")]
    SimpleProxy(#[from] SimpleProxyError),
    #[error("outgoing proxy failed: {0}")]
    OutgoingProxy(#[from] OutgoingProxyError),
    #[error("incoming proxy failed: {0}")]
    IncomingProxy(#[from] IncomingProxyError),
    #[error("files proxy failed: {0}")]
    FilesProxy(#[from] FilesProxyError),
}

pub type Result<T> = core::result::Result<T, IntProxyError>;

pub fn agent_lost_io_error() -> ResponseError {
    ResponseError::RemoteIO(RemoteIOError {
        raw_os_error: None,
        kind: ErrorKindInternal::Unknown("connection with mirrord-agent was lost".to_string()),
    })
}
