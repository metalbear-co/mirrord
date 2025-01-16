use std::{convert::Infallible, io};

use hyper::{upgrade::OnUpgrade, Version};
use mirrord_protocol::{
    tcp::{ChunkedResponse, HttpResponse, InternalHttpBody},
    ConnectionId, Port, RequestId,
};
use thiserror::Error;

#[derive(Debug)]
pub enum InProxyTaskMessage {
    Tcp(Vec<u8>),
    Http(HttpOut),
}

#[derive(Debug)]
pub enum HttpOut {
    ResponseBasic(HttpResponse<Vec<u8>>),
    ResponseFramed(HttpResponse<InternalHttpBody>),
    ResponseChunked(ChunkedResponse),
    Upgraded(OnUpgrade),
}

impl From<Vec<u8>> for InProxyTaskMessage {
    fn from(value: Vec<u8>) -> Self {
        Self::Tcp(value)
    }
}

impl From<HttpOut> for InProxyTaskMessage {
    fn from(value: HttpOut) -> Self {
        Self::Http(value)
    }
}

#[derive(Error, Debug)]
pub enum InProxyTaskError {
    #[error("io failed: {0}")]
    IoError(#[from] io::Error),
    #[error("local HTTP upgrade failed: {0}")]
    UpgradeError(#[source] hyper::Error),
}

impl From<Infallible> for InProxyTaskError {
    fn from(_: Infallible) -> Self {
        unreachable!()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InProxyTask {
    MirrorTcpProxy(ConnectionId),
    StealTcpProxy(ConnectionId),
    HttpGateway(HttpGatewayId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HttpGatewayId {
    pub connection_id: ConnectionId,
    pub request_id: RequestId,
    pub port: Port,
    pub version: Version,
}
