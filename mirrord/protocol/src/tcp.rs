use core::fmt::Display;
use std::{
    collections::VecDeque,
    convert::Infallible,
    fmt,
    net::IpAddr,
    pin::Pin,
    sync::LazyLock,
    task::{Context, Poll},
};

use bincode::{Decode, Encode};
use bytes::Bytes;
use http_body_util::{combinators::BoxBody, BodyExt, Full};
use hyper::{
    body::{Body, Frame, Incoming},
    http,
    http::response::Parts,
    HeaderMap, Method, Request, Response, StatusCode, Uri, Version,
};
use mirrord_macros::protocol_break;
use semver::VersionReq;
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
    HttpRequest(HttpRequest<Vec<u8>>),
    HttpRequestFramed(HttpRequest<InternalHttpBody>),
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
pub enum HttpFilter {
    /// Filter by header ("User-Agent: B")
    Header(Filter),
    /// Filter by path ("/api/v1")
    Path(Filter),
}

impl Display for HttpFilter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HttpFilter::Header(filter) => write!(f, "header={filter}"),
            HttpFilter::Path(filter) => write!(f, "path={filter}"),
        }
    }
}

/// Describes the stealing subscription to a port:
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
#[protocol_break(2)]
pub enum StealType {
    /// Steal all traffic to this port.
    All(Port),
    /// Steal HTTP traffic matching a given filter (header based). - REMOVE THIS WHEN BREAKING
    /// PROTOCOL
    FilteredHttp(Port, Filter),
    /// Steal HTTP traffic matching a given filter - supporting more than once kind of filter
    FilteredHttpEx(Port, HttpFilter),
}

impl StealType {
    pub fn get_port(&self) -> Port {
        let (StealType::All(port)
        | StealType::FilteredHttpEx(port, ..)
        | StealType::FilteredHttp(port, ..)) = self;
        *port
    }
}

/// Messages related to Steal Tcp handler from client.
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum LayerTcpSteal {
    PortSubscribe(StealType),
    ConnectionUnsubscribe(ConnectionId),
    PortUnsubscribe(Port),
    Data(TcpData),
    HttpResponse(HttpResponse<Vec<u8>>),
    HttpResponseFramed(HttpResponse<InternalHttpBody>),
}

/// (De-)Serializable HTTP request.
#[derive(Serialize, Deserialize, PartialEq, Debug, Eq, Clone)]
pub struct InternalHttpRequest<Body> {
    #[serde(with = "http_serde::method")]
    pub method: Method,

    #[serde(with = "http_serde::uri")]
    pub uri: Uri,

    #[serde(with = "http_serde::header_map")]
    pub headers: HeaderMap,

    #[serde(with = "http_serde::version")]
    pub version: Version,

    pub body: Body,
}

impl<E> From<InternalHttpRequest<InternalHttpBody>> for Request<BoxBody<Bytes, E>>
where
    E: From<Infallible>,
{
    fn from(value: InternalHttpRequest<InternalHttpBody>) -> Self {
        let InternalHttpRequest {
            method,
            uri,
            headers,
            version,
            body,
        } = value;
        let mut request = Request::new(BoxBody::new(body.map_err(|e| e.into())));
        *request.method_mut() = method;
        *request.uri_mut() = uri;
        *request.version_mut() = version;
        *request.headers_mut() = headers;

        request
    }
}

impl<E> From<InternalHttpRequest<Vec<u8>>> for Request<BoxBody<Bytes, E>>
where
    E: From<Infallible>,
{
    fn from(value: InternalHttpRequest<Vec<u8>>) -> Self {
        let InternalHttpRequest {
            method,
            uri,
            headers,
            version,
            body,
        } = value;
        let mut request = Request::new(BoxBody::new(
            Full::new(Bytes::from(body)).map_err(|e| e.into()),
        ));
        *request.method_mut() = method;
        *request.uri_mut() = uri;
        *request.version_mut() = version;
        *request.headers_mut() = headers;

        request
    }
}

#[derive(Debug, Clone)]
pub enum HttpRequestFallback {
    Framed(HttpRequest<InternalHttpBody>),
    Fallback(HttpRequest<Vec<u8>>),
}

impl HttpRequestFallback {
    pub fn connection_id(&self) -> ConnectionId {
        match self {
            HttpRequestFallback::Framed(req) => req.connection_id,
            HttpRequestFallback::Fallback(req) => req.connection_id,
        }
    }

    pub fn port(&self) -> Port {
        match self {
            HttpRequestFallback::Framed(req) => req.port,
            HttpRequestFallback::Fallback(req) => req.port,
        }
    }

    pub fn request_id(&self) -> RequestId {
        match self {
            HttpRequestFallback::Framed(req) => req.request_id,
            HttpRequestFallback::Fallback(req) => req.request_id,
        }
    }

    pub fn version(&self) -> Version {
        match self {
            HttpRequestFallback::Framed(req) => req.version(),
            HttpRequestFallback::Fallback(req) => req.version(),
        }
    }

    pub fn into_hyper<E>(self) -> Request<BoxBody<Bytes, E>>
    where
        E: From<Infallible>,
    {
        match self {
            HttpRequestFallback::Framed(req) => req.internal_request.into(),
            HttpRequestFallback::Fallback(req) => req.internal_request.into(),
        }
    }
}

/// Minimal mirrord-protocol version that allows `DaemonTcp::HttpRequestFramed` instead of
/// `DaemonTcp::HttpRequest`.
pub static HTTP_FRAMED_VERSION: LazyLock<VersionReq> =
    LazyLock::new(|| ">=1.3.0".parse().expect("Bad Identifier"));

/// Protocol break - on version 2, please add source port, dest/src IP to the message
/// so we can avoid losing this information.
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
#[protocol_break(2)]
#[bincode(bounds = "for<'de> Body: Serialize + Deserialize<'de>")]
pub struct HttpRequest<Body>
where
    for<'de> Body: Serialize + Deserialize<'de>,
{
    #[bincode(with_serde)]
    pub internal_request: InternalHttpRequest<Body>,
    pub connection_id: ConnectionId,
    pub request_id: RequestId,
    /// Unlike TcpData, HttpRequest includes the port, so that the connection can be created
    /// "lazily", with the first filtered request.
    pub port: Port,
}

impl<B: Serialize> HttpRequest<B>
where
    for<'de> B: Serialize + Deserialize<'de>,
{
    /// Gets this request's HTTP version.
    pub fn version(&self) -> Version {
        self.internal_request.version
    }
}

/// (De-)Serializable HTTP response.
#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
pub struct InternalHttpResponse<Body> {
    #[serde(with = "http_serde::status_code")]
    status: StatusCode,

    #[serde(with = "http_serde::version")]
    version: Version,

    #[serde(with = "http_serde::header_map")]
    headers: HeaderMap,

    body: Body,
}

impl<B> InternalHttpResponse<B> {
    pub fn map_body<T, F>(self, cb: F) -> InternalHttpResponse<T>
    where
        F: FnOnce(B) -> T,
    {
        let InternalHttpResponse {
            status,
            version,
            headers,
            body,
        } = self;

        InternalHttpResponse {
            status,
            version,
            headers,
            body: cb(body),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Default, PartialEq, Eq, Clone)]
pub struct InternalHttpBody(VecDeque<InternalHttpBodyFrame>);

impl InternalHttpBody {
    pub fn from_bytes(bytes: &[u8]) -> Self {
        InternalHttpBody(VecDeque::from([InternalHttpBodyFrame::Data(
            bytes.to_vec(),
        )]))
    }

    pub async fn from_body<B>(mut body: B) -> Result<Self, B::Error>
    where
        B: Body<Data = Bytes> + Unpin,
    {
        let mut frames = VecDeque::new();

        while let Some(frame) = body.frame().await {
            frames.push_back(frame?.into());
        }

        Ok(InternalHttpBody(frames))
    }
}

impl Body for InternalHttpBody {
    type Data = Bytes;

    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        Poll::Ready(self.0.pop_front().map(Frame::from).map(Ok))
    }

    fn is_end_stream(&self) -> bool {
        self.0.is_empty()
    }
}

#[derive(Serialize, Deserialize, PartialEq, Eq, Clone)]
pub enum InternalHttpBodyFrame {
    Data(Vec<u8>),
    Trailers(#[serde(with = "http_serde::header_map")] HeaderMap),
}

impl From<Frame<Bytes>> for InternalHttpBodyFrame {
    fn from(frame: Frame<Bytes>) -> Self {
        if frame.is_data() {
            InternalHttpBodyFrame::Data(frame.into_data().expect("Malfromed data frame").to_vec())
        } else if frame.is_trailers() {
            InternalHttpBodyFrame::Trailers(
                frame.into_trailers().expect("Malfromed trailers frame"),
            )
        } else {
            panic!("Malfromed frame type")
        }
    }
}

impl From<InternalHttpBodyFrame> for Frame<Bytes> {
    fn from(frame: InternalHttpBodyFrame) -> Self {
        match frame {
            InternalHttpBodyFrame::Data(data) => Frame::data(Bytes::from(data)),
            InternalHttpBodyFrame::Trailers(map) => Frame::trailers(map),
        }
    }
}

impl fmt::Debug for InternalHttpBodyFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InternalHttpBodyFrame::Data(data) => f
                .debug_tuple("Data")
                .field(&format_args!("{} (length)", data.len()))
                .finish(),
            InternalHttpBodyFrame::Trailers(map) => {
                f.debug_tuple("Trailers").field(&map.len()).finish()
            }
        }
    }
}

#[derive(Debug)]
pub enum HttpResponseFallback {
    Framed(HttpResponse<InternalHttpBody>),
    Fallback(HttpResponse<Vec<u8>>),
}

impl HttpResponseFallback {
    pub fn connection_id(&self) -> ConnectionId {
        match self {
            HttpResponseFallback::Framed(req) => req.connection_id,
            HttpResponseFallback::Fallback(req) => req.connection_id,
        }
    }

    pub fn request_id(&self) -> RequestId {
        match self {
            HttpResponseFallback::Framed(req) => req.request_id,
            HttpResponseFallback::Fallback(req) => req.request_id,
        }
    }

    pub fn into_hyper<E>(self) -> Result<Response<BoxBody<Bytes, E>>, http::Error>
    where
        E: From<Infallible>,
    {
        match self {
            HttpResponseFallback::Framed(req) => req.internal_response.try_into(),
            HttpResponseFallback::Fallback(req) => req.internal_response.try_into(),
        }
    }

    pub fn response_from_request(
        request: HttpRequestFallback,
        status: StatusCode,
        message: &str,
    ) -> Self {
        match request {
            HttpRequestFallback::Framed(request) => HttpResponseFallback::Framed(
                HttpResponse::<InternalHttpBody>::response_from_request(request, status, message),
            ),
            HttpRequestFallback::Fallback(request) => HttpResponseFallback::Fallback(
                HttpResponse::<Vec<u8>>::response_from_request(request, status, message),
            ),
        }
    }
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
#[bincode(bounds = "for<'de> Body: Serialize + Deserialize<'de>")]
pub struct HttpResponse<Body>
where
    for<'de> Body: Serialize + Deserialize<'de>,
{
    /// This is used to make sure the response is sent in its turn, after responses to all earlier
    /// requests were already sent.
    pub port: Port,
    pub connection_id: ConnectionId,
    pub request_id: RequestId,
    #[bincode(with_serde)]
    pub internal_response: InternalHttpResponse<Body>,
}

impl HttpResponse<InternalHttpBody> {
    /// We cannot implement this with the [`From`] trait as it doesn't support `async` conversions,
    /// and we also need some extra parameters.
    ///
    /// So this is our alternative implementation to `From<Response<Incoming>>`.
    pub async fn from_hyper_response(
        response: Response<Incoming>,
        port: Port,
        connection_id: ConnectionId,
        request_id: RequestId,
    ) -> Result<HttpResponse<InternalHttpBody>, hyper::Error> {
        let (
            Parts {
                status,
                version,
                headers,
                ..
            },
            body,
        ) = response.into_parts();

        let body = InternalHttpBody::from_body(body).await?;

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

    pub fn response_from_request(
        request: HttpRequest<InternalHttpBody>,
        status: StatusCode,
        message: &str,
    ) -> Self {
        let HttpRequest {
            internal_request: InternalHttpRequest { version, .. },
            connection_id,
            request_id,
            port,
        } = request;

        let body = InternalHttpBody::from_bytes(
            format!(
                "{} {}\n{}\n",
                status.as_str(),
                status.canonical_reason().unwrap_or_default(),
                message
            )
            .as_bytes(),
        );

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

    pub fn empty_response_from_request(
        request: HttpRequest<InternalHttpBody>,
        status: StatusCode,
    ) -> Self {
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

impl HttpResponse<Vec<u8>> {
    /// We cannot implement this with the [`From`] trait as it doesn't support `async` conversions,
    /// and we also need some extra parameters.
    ///
    /// So this is our alternative implementation to `From<Response<Incoming>>`.
    pub async fn from_hyper_response(
        response: Response<Incoming>,
        port: Port,
        connection_id: ConnectionId,
        request_id: RequestId,
    ) -> Result<HttpResponse<Vec<u8>>, hyper::Error> {
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

    pub fn response_from_request(
        request: HttpRequest<Vec<u8>>,
        status: StatusCode,
        message: &str,
    ) -> Self {
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

    pub fn empty_response_from_request(request: HttpRequest<Vec<u8>>, status: StatusCode) -> Self {
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

impl<E> TryFrom<InternalHttpResponse<InternalHttpBody>> for Response<BoxBody<Bytes, E>>
where
    E: From<Infallible>,
{
    type Error = http::Error;

    fn try_from(value: InternalHttpResponse<InternalHttpBody>) -> Result<Self, Self::Error> {
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

        builder.body(BoxBody::new(body.map_err(|e| e.into())))
    }
}

impl<E> TryFrom<InternalHttpResponse<Vec<u8>>> for Response<BoxBody<Bytes, E>>
where
    E: From<Infallible>,
{
    type Error = http::Error;

    fn try_from(value: InternalHttpResponse<Vec<u8>>) -> Result<Self, Self::Error> {
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

        builder.body(BoxBody::new(
            Full::new(Bytes::from(body)).map_err(|e| e.into()),
        ))
    }
}
