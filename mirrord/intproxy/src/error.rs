use std::io;

use mirrord_intproxy_protocol::{codec::CodecError, LayerToProxyMessage};
use mirrord_protocol::DaemonMessage;
use thiserror::Error;

use crate::{
    agent_conn::{AgentChannelError, AgentConnectionError},
    layer_initializer::LayerInitializerError,
    ping_pong::PingPongError,
    proxies::{incoming::IncomingProxyError, outgoing::OutgoingProxyError},
    request_queue::RequestQueueEmpty,
    MainTaskId,
};

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
    #[error("agent sent unexpected message: {0:?}")]
    UnexpectedAgentMessage(DaemonMessage),

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
    SimpleProxy(#[from] RequestQueueEmpty),
    #[error("outgoing proxy failed: {0}")]
    OutgoingProxy(#[from] OutgoingProxyError),
    #[error("incoming proxy failed: {0}")]
    IncomingProxy(#[from] IncomingProxyError),
}

pub type Result<T> = core::result::Result<T, IntProxyError>;
