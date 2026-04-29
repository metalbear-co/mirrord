use std::{convert::Infallible, fmt, io};

use hyper::{Version, upgrade::OnUpgrade};
use mirrord_protocol::{ConnectionId, Port, RequestId};
use thiserror::Error;

use super::tls::LocalTlsSetupError;

/// Messages produced by the [`BackgroundTask`](crate::background_tasks::BackgroundTask)s used in
/// the [`IncomingProxy`](super::IncomingProxy).
pub enum InProxyTaskMessage {
    Http(
        /// HTTP spefiic message.
        HttpOut,
    ),
}

impl fmt::Debug for InProxyTaskMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http(msg) => f.debug_tuple("Http").field(msg).finish(),
        }
    }
}

/// Messages produced by the [`HttpGatewayTask`](super::http_gateway::HttpGatewayTask).
#[derive(Debug)]
pub enum HttpOut {
    Upgraded(OnUpgrade),
}

impl From<HttpOut> for InProxyTaskMessage {
    fn from(value: HttpOut) -> Self {
        Self::Http(value)
    }
}

/// Errors that can occur in the [`BackgroundTask`](crate::background_tasks::BackgroundTask)s used
/// in the [`IncomingProxy`](super::IncomingProxy).
///
/// All of these can occur only in the [`TcpProxyTask`](super::tcp_proxy::TcpProxyTask)
/// and mean that the local connection is irreversibly broken.
/// The [`HttpGatewayTask`](super::http_gateway::HttpGatewayTask) produces no errors
/// and instead responds with an error HTTP response to the agent.
///
/// However, due to [`BackgroundTasks`](crate::background_tasks::BackgroundTasks)
/// type constraints, we need a common error type.
/// Thus, this type implements [`From<Infallible>`].
#[derive(Error, Debug)]
pub enum InProxyTaskError {
    #[error("io failed: {0}")]
    Io(#[from] io::Error),
    #[error("local HTTP upgrade failed: {0}")]
    Upgrade(#[source] hyper::Error),
    #[error("failed to prepare TLS client configuration: {0}")]
    TlsSetup(#[from] LocalTlsSetupError),
}

impl From<Infallible> for InProxyTaskError {
    fn from(_: Infallible) -> Self {
        unreachable!()
    }
}

/// Types of [`BackgroundTask`](crate::background_tasks::BackgroundTask)s used in the
/// [`IncomingProxy`](super::IncomingProxy).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InProxyTask {
    /// [`TcpProxyTask`](super::tcp_proxy::TcpProxyTask) handling a mirrored connection.
    MirrorTcpProxy(ConnectionId),
    /// [`TcpProxyTask`](super::tcp_proxy::TcpProxyTask) handling a stolen connection.
    StealTcpProxy(ConnectionId),
    /// [`HttpGatewayTask`](super::http_gateway::HttpGatewayTask) handling a mirrored HTTP request.
    MirrorHttpGateway(HttpGatewayId),
    /// [`HttpGatewayTask`](super::http_gateway::HttpGatewayTask) handling a stolen HTTP request.
    StealHttpGateway(HttpGatewayId),
}

/// Identifies a [`HttpGatewayTask`](super::http_gateway::HttpGatewayTask).
///
/// ([`ConnectionId`], [`RequestId`]) would suffice, but storing extra data allows us to produce an
/// error response in case the task somehow panics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HttpGatewayId {
    /// Id of the remote connection.
    pub connection_id: ConnectionId,
    /// Id of the stolen request.
    pub request_id: RequestId,
    /// Remote port from which the request was stolen.
    pub port: Port,
    /// HTTP version of the stolen request.
    pub version: Version,
}
