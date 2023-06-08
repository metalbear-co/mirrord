use core::fmt::Display;
use std::{fmt, net::IpAddr};

use bincode::{Decode, Encode};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{
    body::Incoming, http, http::response::Parts, HeaderMap, Method, Request, Response, StatusCode,
    Uri, Version,
};
use serde::{Deserialize, Serialize};
use tracing::error;

use crate::{ConnectionId, Port, RemoteResult, RequestId};

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct NewTcpConnection {
    pub connection_id: ConnectionId,
    pub remote_address: IpAddr,
    pub destination_port: Port,
    pub source_port: Port,
    pub local_address: IpAddr,
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

/// Wraps the string that will become a [`fancy_regex::Regex`], providing a nice API in
/// `Filter::new` that validates the regex in mirrord-layer.
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct Filter(String);

impl Filter {
    pub fn new(filter_str: String) -> Result<Self, fancy_regex::Error> {
        let _ = fancy_regex::Regex::new(&filter_str).inspect_err(|fail| {
            error!(
                r"
                Something went wrong while creating a regex for [{filter_str:#?}]!

                >> Please check that the string supplied is a valid regex according to
                   the fancy-regex crate (https://docs.rs/fancy-regex/latest/fancy_regex/).

                > Error:
                {fail:#?}
                "
            )
        })?;

        Ok(Self(filter_str))
    }
}

impl Display for Filter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Display::fmt(&self.0, f)
    }
}

/// Describes different types of HTTP filtering available
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum HTTPFilter {
    /// Filter by header ("User-Agent: B")
    Header(Filter),
    /// Filter by path ("/api/v1")
    Path(Filter),
}

/// Describes the stealing subscription to a port:
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum StealType {
    /// Steal all traffic to this port.
    All(Port),
    /// Steal HTTP traffic matching a given filter.
    FilteredHttp(Port, Filter),
    /// Steal HTTP traffic matching a given filter - supporting more than once kind of filter
    FilteredHttpV2(Port, HTTPFilter),
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
#[derive(Serialize, Deserialize, PartialEq, Eq, Clone)]
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
}

impl fmt::Debug for InternalHttpRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InternalHttpRequest")
            .field("method", &self.method)
            .field("uri", &self.uri)
            .field("headers", &self.headers)
            .field("version", &self.version)
            .field("body (length)", &self.body.len())
            .finish()
    }
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
        *request.method_mut() = method;
        *request.uri_mut() = uri;
        *request.version_mut() = version;
        *request.headers_mut() = headers;

        request
    }
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct HttpRequest {
    #[bincode(with_serde)]
    pub internal_request: InternalHttpRequest,
    pub connection_id: ConnectionId,
    pub request_id: RequestId,
    /// Unlike TcpData, HttpRequest includes the port, so that the connection can be created
    /// "lazily", with the first filtered request.
    pub port: Port,
}

impl HttpRequest {
    /// Gets this request's HTTP version.
    pub fn version(&self) -> Version {
        self.internal_request.version
    }
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
    pub internal_response: InternalHttpResponse,
}

impl HttpResponse {
    /// We cannot implement this with the [`From`] trait as it doesn't support `async` conversions,
    /// and we also need some extra parameters.
    ///
    /// So this is our alternative implementation to `From<Response<Incoming>>`.
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
                ..
            },
            body,
        ) = response.into_parts();

        let body = body.collect().await?.to_bytes().to_vec();
        let internal_response = InternalHttpResponse {
            status,
            headers,
            version,
            body,
        };

        Ok(HttpResponse {
            request_id,
            port,
            connection_id,
            internal_response,
        })
    }

    pub fn response_from_request(request: HttpRequest, status: StatusCode, message: &str) -> Self {
        let HttpRequest {
            internal_request: InternalHttpRequest { version, .. },
            connection_id,
            request_id,
            port,
        } = request;

        let body = format!(
            "{} {}\n{}\n",
            status.as_str(),
            status.canonical_reason().unwrap_or_default(),
            message
        )
        .into_bytes();

        Self {
            port,
            connection_id,
            request_id,
            internal_response: InternalHttpResponse {
                status,
                version,
                headers: Default::default(),
                body,
            },
        }
    }

    pub fn empty_response_from_request(request: HttpRequest, status: StatusCode) -> Self {
        let HttpRequest {
            internal_request: InternalHttpRequest { version, .. },
            connection_id,
            request_id,
            port,
        } = request;

        Self {
            port,
            connection_id,
            request_id,
            internal_response: InternalHttpResponse {
                status,
                version,
                headers: Default::default(),
                body: Default::default(),
            },
        }
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
