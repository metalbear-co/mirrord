use mirrord_intproxy_protocol::{LayerToProxyMessage, codec::CodecError};
use mirrord_protocol::{DaemonMessage, ErrorKindInternal, RemoteIOError, ResponseError};
use thiserror::Error;

use crate::{
    MainTaskId,
    agent_conn::{AgentConnectionError, AgentConnectionTaskError},
    layer_initializer::LayerInitializerError,
    ping_pong::PingPongError,
    proxies::{
        files::FilesProxyError, incoming::IncomingProxyError, outgoing::OutgoingProxyError,
        simple::SimpleProxyError,
    },
};

#[derive(Error, Debug)]
#[error("agent sent an unexpected message: {0:?}")]
pub struct UnexpectedAgentMessage(pub DaemonMessage);

/// This kind of error causes a partial failure of the proxy, meaning that for these errors does
/// exist a failover strategy, so in case of incurring of this error, the proxy change behavior,
/// according to [`crate::FailoverStrategy`]
#[derive(Error, Debug)]
pub(crate) enum ProxyRuntimeError {
    #[error("layer sent unexpected message: {0:?}")]
    UnexpectedLayerMessage(LayerToProxyMessage),

    #[error("connection with agent failed: {0}")]
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
    AgentChannel(#[from] AgentConnectionTaskError),
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

/// This kind of error causes a total failure of the proxy, meaning that for these errors doesn't
/// exist a failover strategy, so facing this error the proxy stops working.
#[derive(Error, Debug)]
pub enum ProxyStartupError {
    #[error("waiting for the first layer connection timed out")]
    ConnectionAcceptTimeout,
}

pub fn agent_lost_io_error() -> ResponseError {
    ResponseError::RemoteIO(RemoteIOError {
        raw_os_error: None,
        kind: ErrorKindInternal::Unknown("connection with mirrord-agent was lost".to_string()),
    })
}
