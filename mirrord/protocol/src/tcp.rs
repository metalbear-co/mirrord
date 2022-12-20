use std::{cmp::Ordering, fmt, net::IpAddr};

use bincode::{Decode, Encode};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{
    body::Incoming, http, http::response::Parts, HeaderMap, Method, Request, Response, StatusCode,
    Uri, Version,
};
use serde::{Deserialize, Serialize};

use crate::{ConnectionId, Port, RemoteResult, RequestId};

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct NewTcpConnection {
    pub connection_id: ConnectionId,
    pub address: IpAddr,
    pub destination_port: Port,
    pub source_port: Port,
}

#[derive(Encode, Decode, PartialEq, Eq, Clone)]
pub struct TcpData {
    pub connection_id: ConnectionId,
    pub bytes: Vec<u8>,
}

impl fmt::Debug for TcpData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TcpData")
            .field("connection_id", &self.connection_id)
            .field("bytes (length)", &self.bytes.len())
            .finish()
    }
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct TcpClose {
    pub connection_id: ConnectionId,
}

/// Messages related to Tcp handler from client.
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum LayerTcp {
    PortSubscribe(Port),
    ConnectionUnsubscribe(ConnectionId),
    PortUnsubscribe(Port),
}

/// Messages related to Tcp handler from server.
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum DaemonTcp {
    NewConnection(NewTcpConnection),
    Data(TcpData),
    Close(TcpClose),
    /// Used to notify the subscription occured, needed for e2e tests to remove sleeps and
    /// flakiness.
    SubscribeResult(RemoteResult<Port>),
    HttpRequest(HttpRequest),
}

/// Describes the stealing subscription to a port:
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum StealType {
    /// Steal all traffic to this port.
    All(Port),
    /// Steal HTTP traffic matching a given filter.
    FilteredHttp(Port, String),
}

/// Messages related to Steal Tcp handler from client.
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum LayerTcpSteal {
    PortSubscribe(StealType),
    ConnectionUnsubscribe(ConnectionId),
    PortUnsubscribe(Port),
    Data(TcpData),
    HttpResponse(HttpResponse),
}

/// (De-)Serializable HTTP request.
#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
pub struct InternalHttpRequest {
    #[serde(with = "http_serde::method")]
    pub method: Method,

    #[serde(with = "http_serde::uri")]
    pub uri: Uri,

    #[serde(with = "http_serde::header_map")]
    pub headers: HeaderMap,

    #[serde(with = "http_serde::version")]
    pub version: Version,

    pub body: Vec<u8>,
    // TODO: What about `extensions`? There is no `http_serde` method for it but it is in `Parts`.
}

impl From<InternalHttpRequest> for Request<Full<Bytes>> {
    fn from(value: InternalHttpRequest) -> Self {
        let InternalHttpRequest {
            method,
            uri,
            headers,
            version,
            body,
        } = value;
        let mut request = Request::new(Full::new(Bytes::from(body)));
        // TODO: can we construct the request with those values instead of constructing, then
        //       setting? Does it matter?
        *request.method_mut() = method;
        *request.uri_mut() = uri;
        *request.version_mut() = version;
        *request.headers_mut() = headers;
        // TODO: extensions?

        request
    }
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct HttpRequest {
    #[bincode(with_serde)]
    pub request: InternalHttpRequest,
    pub connection_id: ConnectionId,
    pub request_id: RequestId,
    /// Unlike TcpData, HttpRequest includes the port, so that the connection can be created
    /// "lazily", with the first filtered request.
    pub port: Port,
}

/// (De-)Serializable HTTP response.
#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
pub struct InternalHttpResponse {
    #[serde(with = "http_serde::status_code")]
    status: StatusCode,

    #[serde(with = "http_serde::version")]
    version: Version,

    #[serde(with = "http_serde::header_map")]
    headers: HeaderMap,

    body: Vec<u8>,
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct HttpResponse {
    /// This is used to make sure the response is sent in its turn, after responses to all earlier
    /// requests were already sent.
    pub port: Port,
    pub connection_id: ConnectionId,
    pub request_id: RequestId,
    #[bincode(with_serde)]
    pub response: InternalHttpResponse,
}

impl PartialOrd<Self> for HttpResponse {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// An total order which a request with a LESSER ID is GREATER.
impl Ord for HttpResponse {
    /// request1 > request2  iff. request1.request_id < request2.request_id.
    fn cmp(&self, other: &Self) -> Ordering {
        // Reversed order because we want to process lower request_ids first, so we want a min heap
        // instead of the default max heap.
        other.request_id.cmp(&self.request_id)
    }
}

// Not implemented as From<Response<Incoming>> because async.
impl HttpResponse {
    pub async fn from_hyper_response(
        response: Response<Incoming>,
        port: Port,
        connection_id: ConnectionId,
        request_id: RequestId,
    ) -> Result<HttpResponse, hyper::Error> {
        let (
            Parts {
                status,
                version,
                headers,
                extensions: _, // TODO: do we need to use it? There is not such `http_serde` method.
                ..
            },
            body,
        ) = response.into_parts();
        let body = body.collect().await?.to_bytes().to_vec();
        let internal_req = InternalHttpResponse {
            status,
            headers,
            version,
            body,
        };
        Ok(HttpResponse {
            request_id,
            port,
            connection_id,
            response: internal_req,
        })
    }
}

impl TryFrom<InternalHttpResponse> for Response<Full<Bytes>> {
    type Error = http::Error;

    fn try_from(value: InternalHttpResponse) -> Result<Self, Self::Error> {
        let InternalHttpResponse {
            status,
            version,
            headers,
            body,
        } = value;

        let mut builder = Response::builder().status(status).version(version);
        if let Some(h) = builder.headers_mut() {
            *h = headers;
        }
        builder.body(Full::new(Bytes::from(body)))
    }
}
