use core::fmt::Display;
use std::{
    collections::VecDeque,
    convert::Infallible,
    fmt,
    net::{IpAddr, SocketAddr},
    pin::Pin,
    sync::LazyLock,
    task::{Context, Poll},
};

use bincode::{Decode, Encode};
use bytes::Bytes;
use http_body_util::BodyExt;
use hyper::{
    body::{Body, Frame},
    HeaderMap, Method, Request, Response, StatusCode, Uri, Version,
};
use mirrord_macros::protocol_break;
use semver::VersionReq;
use serde::{Deserialize, Serialize};

use crate::{ConnectionId, Payload, Port, RemoteResult, RequestId};

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct NewTcpConnectionV1 {
    pub connection_id: ConnectionId,
    pub remote_address: IpAddr,
    pub destination_port: Port,
    pub source_port: Port,
    pub local_address: IpAddr,
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct NewTcpConnectionV2 {
    pub connection: NewTcpConnectionV1,
    pub transport: IncomingTrafficTransportType,
}

#[derive(Encode, Decode, PartialEq, Eq, Clone)]
pub struct TcpData {
    pub connection_id: ConnectionId,
    pub bytes: Payload,
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
///
/// Part of the `mirror` feature.
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum LayerTcp {
    /// User is interested in mirroring traffic on this `Port`, so add it to the list of
    /// ports that the sniffer is filtering.
    PortSubscribe(Port),

    /// User is not interested in the connection with `ConnectionId` anymore.
    ///
    /// This means that their app has closed the connection they were `listen`ning on.
    ///
    /// There is no `ConnectionSubscribe` counter-part of this variant, the subscription
    /// happens when the sniffer receives an (agent) internal `SniffedConnection`.
    ConnectionUnsubscribe(ConnectionId),

    /// Removes this `Port` from the sniffer's filter, the traffic won't be cloned to mirrord
    /// anymore.
    PortUnsubscribe(Port),
}

/// Messages related to Tcp handler from server.
///
/// They are the same for both `steal` and `mirror` modes, even though their layer
/// counterparts ([`LayerTcpSteal`] and [`LayerTcp`]) are different.
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum DaemonTcp {
    NewConnectionV1(NewTcpConnectionV1),
    Data(TcpData),
    Close(TcpClose),
    /// Used to notify the subscription occured, needed for e2e tests to remove sleeps and
    /// flakiness.
    SubscribeResult(RemoteResult<Port>),
    HttpRequest(HttpRequest<Payload>),
    HttpRequestFramed(HttpRequest<InternalHttpBody>),
    HttpRequestChunked(ChunkedRequest),
    NewConnectionV2(NewTcpConnectionV2),
}

/// Contents of a chunked message from server.
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum ChunkedRequest {
    StartV1(ChunkedRequestStartV1),
    Body(ChunkedRequestBodyV1),
    ErrorV1(ChunkedRequestErrorV1),
    StartV2(ChunkedRequestStartV2),
    ErrorV2(ChunkedRequestErrorV2),
}

pub type ChunkedRequestStartV1 = HttpRequest<Vec<InternalHttpBodyFrame>>;

/// Contents of a chunked message body frame from server.
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct ChunkedRequestBodyV1 {
    #[bincode(with_serde)]
    pub frames: Vec<InternalHttpBodyFrame>,
    pub is_last: bool,
    pub connection_id: ConnectionId,
    pub request_id: RequestId,
}

impl From<InternalHttpBodyFrame> for Frame<Bytes> {
    fn from(value: InternalHttpBodyFrame) -> Self {
        match value {
            InternalHttpBodyFrame::Data(data) => Frame::data(data.0),
            InternalHttpBodyFrame::Trailers(map) => Frame::trailers(map),
        }
    }
}

/// An error occurred while processing chunked data from server.
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct ChunkedRequestErrorV1 {
    pub connection_id: ConnectionId,
    pub request_id: RequestId,
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct ChunkedRequestStartV2 {
    pub connection_id: ConnectionId,
    pub request_id: RequestId,
    #[bincode(with_serde)]
    pub request: InternalHttpRequest<InternalHttpBodyNew>,
    pub metadata: HttpRequestMetadata,
    pub transport: IncomingTrafficTransportType,
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum IncomingTrafficTransportType {
    Tcp,
    Tls {
        alpn_protocol: Option<Vec<u8>>,
        server_name: Option<String>,
    },
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum HttpRequestMetadata {
    V1 {
        source: SocketAddr,
        destination: SocketAddr,
    },
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct ChunkedRequestErrorV2 {
    pub connection_id: ConnectionId,
    pub request_id: RequestId,
    pub error_message: String,
}

/// Wraps the string that will become a [`fancy_regex::Regex`], providing a nice API in
/// `Filter::new` that validates the regex in mirrord-layer.
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct Filter(String);

impl Filter {
    pub fn new(filter_str: String) -> Result<Self, Box<fancy_regex::Error>> {
        let _ = fancy_regex::Regex::new(&filter_str).inspect_err(|fail| {
            tracing::error!(
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

impl std::ops::Deref for Filter {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Describes different types of HTTP filtering available
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum HttpFilter {
    /// Filter by header ("User-Agent: B")
    Header(Filter),
    /// Filter by path ("/api/v1")
    Path(Filter),
    /// Filter by multiple filters
    Composite {
        /// If true, all filters must match, otherwise any filter can match
        all: bool,
        /// Filters to use
        filters: Vec<HttpFilter>,
    },
}

impl Display for HttpFilter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HttpFilter::Header(filter) => write!(f, "header={filter}"),
            HttpFilter::Path(filter) => write!(f, "path={filter}"),
            HttpFilter::Composite { all, filters } => match all {
                true => {
                    write!(f, "all of ")?;
                    let mut first = true;
                    for filter in filters {
                        if first {
                            write!(f, "({filter})")?;
                            first = false;
                        } else {
                            write!(f, ", ({filter})")?;
                        }
                    }
                    Ok(())
                }
                false => {
                    write!(f, "any of ")?;
                    let mut first = true;
                    for filter in filters {
                        if first {
                            write!(f, "({filter})")?;
                            first = false;
                        } else {
                            write!(f, ", ({filter})")?;
                        }
                    }
                    Ok(())
                }
            },
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
///
/// `PortSubscribe`, `PortUnsubscribe`, and `ConnectionUnsubscribe` variants are similar
/// to what you'll find in the [`LayerTcp`], but they're handled by different tasks in
/// the agent.
///
/// Stolen traffic might have an additional overhead when compared to mirrored traffic, as
/// we have an intermmediate HTTP server to handle filtering (based on HTTP headers, etc).
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum LayerTcpSteal {
    /// User is interested in stealing traffic on this `Port`, so add it to the list of
    /// ports that the stealer is filtering.
    ///
    /// The `TcpConnectionStealer` supports an [`HttpFilter`] granting the ability to steal
    /// only traffic that matches the user configured filter. It's also possible to just steal
    /// all traffic (which we refer as `Unfiltered`). For more info see [`StealType`].
    ///
    /// This variant is somewhat related to [`LayerTcpSteal::ConnectionUnsubscribe`], since
    /// we don't have a `ConnectionSubscribe` message anywhere, instead what we do is: when
    /// a new connection comes in one of the ports we are subscribed to, we consider it a
    /// connection subscription (so this mechanism represents the **non-existing**
    /// `ConnectionSubscribe` variant).
    PortSubscribe(StealType),

    /// User has stopped stealing from this connection with [`ConnectionId`].
    ///
    /// We do **not** have a `ConnectionSubscribe` variant/message. What happens instead is that we
    /// call a _connection subscription_ the act of `accept`ing a new connection on one of the
    /// ports we are subscribed to. See the [`LayerTcpSteal::PortSubscribe`] for more info.
    ConnectionUnsubscribe(ConnectionId),

    /// Removes this `Port` from the stealers's filter, the traffic won't be stolen by mirrord
    /// anymore.
    PortUnsubscribe(Port),
    Data(TcpData),
    HttpResponse(HttpResponse<Payload>),
    HttpResponseFramed(HttpResponse<InternalHttpBody>),
    HttpResponseChunked(ChunkedResponse),
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum ChunkedResponse {
    Start(HttpResponse<Vec<InternalHttpBodyFrame>>),
    Body(ChunkedRequestBodyV1),
    Error(ChunkedRequestErrorV1),
}

/// (De-)Serializable HTTP request.
#[derive(Serialize, Deserialize, PartialEq, Eq, Clone)]
pub struct InternalHttpRequest<B> {
    #[serde(with = "http_serde::method")]
    pub method: Method,

    #[serde(with = "http_serde::uri")]
    pub uri: Uri,

    #[serde(with = "http_serde::header_map")]
    pub headers: HeaderMap,

    #[serde(with = "http_serde::version")]
    pub version: Version,

    pub body: B,
}

impl<B> InternalHttpRequest<B> {
    pub fn map_body<T, F>(self, cb: F) -> InternalHttpRequest<T>
    where
        F: FnOnce(B) -> T,
    {
        let InternalHttpRequest {
            version,
            headers,
            method,
            uri,
            body,
        } = self;

        InternalHttpRequest {
            version,
            headers,
            method,
            uri,
            body: cb(body),
        }
    }
}

impl<B> From<InternalHttpRequest<B>> for Request<B> {
    fn from(value: InternalHttpRequest<B>) -> Self {
        let InternalHttpRequest {
            method,
            uri,
            headers,
            version,
            body,
        } = value;

        let mut request = Request::new(body);
        *request.method_mut() = method;
        *request.uri_mut() = uri;
        *request.version_mut() = version;
        *request.headers_mut() = headers;

        request
    }
}

impl<B: fmt::Debug> fmt::Debug for InternalHttpRequest<B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InternalHttpRequest")
            .field("method", &self.method)
            .field("uri", &self.uri)
            .field("headers", &self.headers.len())
            .field("version", &self.version)
            .field("body", &self.body)
            .finish()
    }
}

/// Minimal mirrord-protocol version that allows [`DaemonTcp::HttpRequestFramed`] and
/// [`LayerTcpSteal::HttpResponseFramed`].
pub static HTTP_FRAMED_VERSION: LazyLock<VersionReq> =
    LazyLock::new(|| ">=1.3.0".parse().expect("Bad Identifier"));

/// Minimal mirrord-protocol version that allows [`DaemonTcp::HttpRequestChunked`].
pub static HTTP_CHUNKED_REQUEST_VERSION: LazyLock<VersionReq> =
    LazyLock::new(|| ">=1.7.0".parse().expect("Bad Identifier"));

/// Minimal mirrord-protocol version that allows [`LayerTcpSteal::HttpResponseChunked`].
pub static HTTP_CHUNKED_RESPONSE_VERSION: LazyLock<VersionReq> =
    LazyLock::new(|| ">=1.8.1".parse().expect("Bad Identifier"));

/// Minimal mirrord-protocol version that allows for v2 variants of [`ChunkedRequest`].
pub static HTTP_CHUNKED_REQUEST_V2_VERSION: LazyLock<VersionReq> =
    LazyLock::new(|| ">=1.19.0".parse().expect("Bad Identifier"));

/// Minimal mirrord-protocol version that allows [`DaemonTcp::Data`] to be sent in the same
/// connection as
/// [`DaemonTcp::HttpRequestChunked`]/[`DaemonTcp::HttpRequestFramed`]/[`DaemonTcp::HttpRequest`].
pub static HTTP_FILTERED_UPGRADE_VERSION: LazyLock<VersionReq> =
    LazyLock::new(|| ">=1.5.0".parse().expect("Bad Identifier"));

/// Minimal mirrord-protocol version that allows [`HttpFilter::Composite`]
pub static HTTP_COMPOSITE_FILTER_VERSION: LazyLock<VersionReq> =
    LazyLock::new(|| ">=1.11.0".parse().expect("Bad Identifier"));

/// Minimal mirrord-protocol version that allows:
/// 1. [`DaemonTcp::NewConnectionV2`]
/// 2. Passing HTTP requests in [`DaemonMessage::Tcp`](crate::DaemonMessage::Tcp)
/// 3. Passing HTTP requests in [`DaemonTcp`] when the client makes an unflitered port subscription
pub static MODE_AGNOSTIC_HTTP_REQUESTS: LazyLock<VersionReq> =
    LazyLock::new(|| ">=1.19.4".parse().expect("Bad Identifier"));

/// Protocol break - on version 2, please add source port, dest/src IP to the message
/// so we can avoid losing this information.
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
#[protocol_break(2)]
#[bincode(bounds = "for<'de> Body: Serialize + Deserialize<'de>")]
pub struct HttpRequest<Body> {
    #[bincode(with_serde)]
    pub internal_request: InternalHttpRequest<Body>,
    pub connection_id: ConnectionId,
    pub request_id: RequestId,
    /// Unlike TcpData, HttpRequest includes the port, so that the connection can be created
    /// "lazily", with the first filtered request.
    pub port: Port,
}

impl<B> HttpRequest<B> {
    /// Gets this request's HTTP version.
    pub fn version(&self) -> Version {
        self.internal_request.version
    }

    pub fn map_body<T, F>(self, map: F) -> HttpRequest<T>
    where
        F: FnOnce(B) -> T,
    {
        HttpRequest {
            connection_id: self.connection_id,
            request_id: self.request_id,
            port: self.port,
            internal_request: InternalHttpRequest {
                method: self.internal_request.method,
                uri: self.internal_request.uri,
                headers: self.internal_request.headers,
                version: self.internal_request.version,
                body: map(self.internal_request.body),
            },
        }
    }
}

/// (De-)Serializable HTTP response.
#[derive(Serialize, Deserialize, PartialEq, Eq, Clone)]
pub struct InternalHttpResponse<Body> {
    #[serde(with = "http_serde::status_code")]
    pub status: StatusCode,

    #[serde(with = "http_serde::version")]
    pub version: Version,

    #[serde(with = "http_serde::header_map")]
    pub headers: HeaderMap,

    pub body: Body,
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

impl<B> From<InternalHttpResponse<B>> for Response<B> {
    fn from(value: InternalHttpResponse<B>) -> Self {
        let InternalHttpResponse {
            status,
            version,
            headers,
            body,
        } = value;

        let mut response = Response::new(body);
        *response.status_mut() = status;
        *response.version_mut() = version;
        *response.headers_mut() = headers;

        response
    }
}

impl<B: fmt::Debug> fmt::Debug for InternalHttpResponse<B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InternalHttpResponse")
            .field("status", &self.status)
            .field("version", &self.version)
            .field("headers", &self.headers.len())
            .field("body", &self.body)
            .finish()
    }
}

#[derive(Serialize, Deserialize, Debug, Default, PartialEq, Eq, Clone)]
pub struct InternalHttpBody(pub VecDeque<InternalHttpBodyFrame>);

impl InternalHttpBody {
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
    Data(Payload),
    Trailers(#[serde(with = "http_serde::header_map")] HeaderMap),
}

impl From<Frame<Bytes>> for InternalHttpBodyFrame {
    fn from(frame: Frame<Bytes>) -> Self {
        frame
            .into_data()
            .map(|bytes| Self::Data(bytes.into()))
            .or_else(|frame| frame.into_trailers().map(Self::Trailers))
            .expect("malformed frame type")
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

#[derive(Serialize, Deserialize, PartialEq, Eq, Clone, Debug)]
pub struct InternalHttpBodyNew {
    pub frames: Vec<InternalHttpBodyFrame>,
    pub is_last: bool,
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
#[bincode(bounds = "for<'de> B: Serialize + Deserialize<'de>")]
pub struct HttpResponse<B> {
    /// This is used to make sure the response is sent in its turn, after responses to all earlier
    /// requests were already sent.
    pub port: Port,
    pub connection_id: ConnectionId,
    pub request_id: RequestId,
    #[bincode(with_serde)]
    pub internal_response: InternalHttpResponse<B>,
}

impl<B> HttpResponse<B> {
    pub fn map_body<T, F>(self, cb: F) -> HttpResponse<T>
    where
        F: FnOnce(B) -> T,
    {
        HttpResponse {
            connection_id: self.connection_id,
            request_id: self.request_id,
            port: self.port,
            internal_response: self.internal_response.map_body(cb),
        }
    }
}
