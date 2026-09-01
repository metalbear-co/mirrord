use std::{fmt, io, net::SocketAddr, ops::Not};

use hyper::{
    Method, Request, Response, StatusCode, Uri, Version,
    body::Incoming,
    client::conn::{http1, http2},
    header::{HOST, HeaderValue},
    http::uri::PathAndQuery,
};
use hyper_util::rt::{TokioExecutor, TokioIo};
use mirrord_protocol::{
    ConnectionId, Payload, Port, RequestId,
    tcp::{HttpRequest, HttpResponse, InternalHttpResponse},
};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite};
use tracing::Level;

mod client_store;
mod response_mode;
mod streaming_body;

pub use client_store::ClientStore;
pub use response_mode::ResponseMode;
pub use streaming_body::StreamingBody;

use super::tls::LocalTlsSetupError;

/// An HTTP client used to pass requests to the user application.
pub struct LocalHttpClient {
    /// Established HTTP connection with the user application.
    sender: HttpSender,
    /// Address of the user application's HTTP server.
    local_server_address: SocketAddr,
    /// Address of this client's TCP socket.
    address: SocketAddr,
    /// Whether this client uses TLS.
    uses_tls: bool,
}

impl LocalHttpClient {
    /// Send the given `request` to the user application's HTTP server.
    #[tracing::instrument(level = Level::TRACE, err(level = Level::TRACE), ret)]
    pub async fn send_request(
        &mut self,
        request: HttpRequest<StreamingBody>,
    ) -> Result<Response<Incoming>, LocalHttpError> {
        self.sender.send_request(request).await
    }

    /// Returns the address of the local server to which this client is connected.
    pub fn local_server_address(&self) -> SocketAddr {
        self.local_server_address
    }

    pub fn handles_version(&self, version: Version) -> bool {
        match (&self.sender, version) {
            (_, Version::HTTP_3) => false,
            (HttpSender::V2(..), Version::HTTP_2) => true,
            (HttpSender::V1(..), _) => true,
            (HttpSender::V2(..), _) => false,
        }
    }

    pub fn uses_tls(&self) -> bool {
        self.uses_tls
    }

    /// Whether the connection with the user application's HTTP server is gone.
    ///
    /// Such a client can never deliver another request - the send fails immediately, reporting a
    /// closed channel.
    pub fn is_closed(&self) -> bool {
        self.sender.is_closed()
    }
}

impl fmt::Debug for LocalHttpClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalHttpClient")
            .field("local_server_address", &self.local_server_address)
            .field("address", &self.address)
            .field("is_http_1", &matches!(self.sender, HttpSender::V1(..)))
            .field("uses_tls", &self.uses_tls)
            .finish()
    }
}

/// Errors that can occur when sending an HTTP request to the user application.
#[derive(Error, Debug)]
pub enum LocalHttpError {
    #[error("failed to make an HTTP handshake with the local application's HTTP server: {0}")]
    HandshakeFailed(#[source] hyper::Error),

    #[error("{0:?} is not supported in the local HTTP proxy")]
    UnsupportedHttpVersion(Version),

    #[error("failed to send the request to the local application's HTTP server: {0}")]
    SendFailed(#[source] hyper::Error),

    #[error("failed to prepare a local TCP socket: {0}")]
    SocketSetupFailed(#[source] io::Error),

    #[error("failed to make a TCP connection with the local application's HTTP server: {0}")]
    ConnectTcpFailed(#[source] io::Error),

    #[error("failed to make a TLS connection with the local application's HTTP server: {0}")]
    ConnectTlsFailed(#[source] io::Error),

    #[error("failed to read the body of the local application's HTTP server response: {0}")]
    ReadBodyFailed(#[source] hyper::Error),

    #[error("failed to prepare TLS client configuration: {0}")]
    TlsSetupError(#[from] LocalTlsSetupError),
}

impl LocalHttpError {
    /// Whether this error means the connection was already gone when the request was sent.
    ///
    /// The request never reached the local application, and the remedy is a new connection, which
    /// costs about a millisecond. Backing off first would add that wait to the latency of a
    /// request that nothing is wrong with.
    ///
    /// Deliberately limited to [`Self::SendFailed`]: a connection lost while the response body is
    /// being read means the local application may have already processed the request, so retrying
    /// it is not safe.
    pub fn is_connection_closed(&self) -> bool {
        match self {
            Self::SendFailed(error) => error.is_closed(),
            _ => false,
        }
    }

    /// Checks if we can retry sending the request, given that the previous attempt resulted in this
    /// error.
    pub fn can_retry(&self) -> bool {
        match self {
            Self::SocketSetupFailed(..)
            | Self::UnsupportedHttpVersion(..)
            | Self::TlsSetupError(..) => false,
            Self::ConnectTcpFailed(..) | Self::ConnectTlsFailed(..) => true,
            Self::HandshakeFailed(err) | Self::SendFailed(err) | Self::ReadBodyFailed(err) => (err
                .is_parse()
                || err.is_parse_status()
                || err.is_parse_too_large()
                || err.is_user())
            .not(),
        }
    }
}

/// Produces a mirrord-specific [`StatusCode::BAD_GATEWAY`] response.
pub fn mirrord_error_response<M: fmt::Display>(
    message: M,
    version: Version,
    connection_id: ConnectionId,
    request_id: RequestId,
    port: Port,
) -> HttpResponse<Payload> {
    let body = format!(
        "mirrord-intproxy v{}: {message}\n",
        env!("CARGO_PKG_VERSION")
    )
    .into_bytes();
    let body = Payload::from(body);
    HttpResponse {
        connection_id,
        port,
        request_id,
        internal_response: InternalHttpResponse {
            status: StatusCode::BAD_GATEWAY,
            version,
            headers: Default::default(),
            body,
        },
    }
}

/// Adapts a request taken from an HTTP/2 connection to be sent over HTTP/1.
///
/// HTTP/2 carries the target in the `:scheme`, `:authority` and `:path` pseudo-headers, which
/// [`hyper`] exposes as a URI in absolute form, and carries no `Host` header. HTTP/1.1 requires
/// `Host`, and [RFC 9113 section 8.3.1] makes recreating it from the authority the job of whoever
/// converts the request. Servers that enforce the requirement (Tomcat, for one) answer a request
/// without it with a bare 400, before the application sees anything.
///
/// The target is rewritten to origin form for a related reason: absolute form is meant for
/// requests made to a proxy, and frameworks that route on the raw target do not expect it.
///
/// A request that already carries a `Host` header keeps it, and a target with no authority to take
/// the host from is left as it is.
///
/// [RFC 9113 section 8.3.1]: https://www.rfc-editor.org/rfc/rfc9113#section-8.3.1
fn downgrade_to_http1<B>(request: &mut Request<B>) {
    if request.version() != Version::HTTP_2 {
        return;
    }

    // In a CONNECT request the authority is the target itself, and there is no path to fall back
    // on.
    if request.method() == Method::CONNECT {
        return;
    }

    *request.version_mut() = Version::HTTP_11;

    let Some(authority) = request.uri().authority().cloned() else {
        return;
    };

    if request.headers().contains_key(HOST).not() {
        // An `Authority` can carry the deprecated userinfo component, which must not appear in a
        // `Host` header.
        let host = authority.as_str();
        let host = host.rsplit_once('@').map_or(host, |(_, host)| host);

        if let Ok(value) = HeaderValue::from_str(host) {
            request.headers_mut().insert(HOST, value);
        }
    }

    let mut parts = request.uri().clone().into_parts();
    parts.scheme = None;
    parts.authority = None;
    parts
        .path_and_query
        .get_or_insert_with(|| PathAndQuery::from_static("/"));
    if let Ok(uri) = Uri::from_parts(parts) {
        *request.uri_mut() = uri;
    }
}

/// Holds either [`http1::SendRequest`] or [`http2::SendRequest`] and exposes a unified interface.
enum HttpSender {
    V1(http1::SendRequest<StreamingBody>),
    V2(http2::SendRequest<StreamingBody>),
}

impl HttpSender {
    /// Performs an HTTP handshake over the given IO stream.
    async fn handshake<IO>(version: Version, target_stream: IO) -> Result<Self, LocalHttpError>
    where
        IO: 'static + AsyncRead + AsyncWrite + Unpin + Send,
    {
        match version {
            Version::HTTP_2 => {
                let (sender, connection) =
                    http2::handshake(TokioExecutor::default(), TokioIo::new(target_stream))
                        .await
                        .map_err(LocalHttpError::HandshakeFailed)?;

                tokio::spawn(async move {
                    match connection.await {
                        Ok(()) => {
                            tracing::trace!("HTTP connection with the local application finished");
                        }
                        Err(error) => {
                            tracing::warn!(%error, "HTTP connection with the local application failed");
                        }
                    }
                });

                Ok(HttpSender::V2(sender))
            }

            Version::HTTP_3 => Err(LocalHttpError::UnsupportedHttpVersion(version)),

            _http_v1 => {
                let (sender, connection) = http1::handshake(TokioIo::new(target_stream))
                    .await
                    .map_err(LocalHttpError::HandshakeFailed)?;

                tokio::spawn(async move {
                    match connection.with_upgrades().await {
                        Ok(()) => {
                            tracing::trace!("HTTP connection with the local application finished");
                        }
                        Err(error) => {
                            tracing::warn!(%error, "HTTP connection with the local application failed");
                        }
                    }
                });

                Ok(HttpSender::V1(sender))
            }
        }
    }

    /// Tries to send the given [`HttpRequest`] to the server.
    /// Whether the connection backing this sender is gone.
    fn is_closed(&self) -> bool {
        match self {
            Self::V1(sender) => sender.is_closed(),
            Self::V2(sender) => sender.is_closed(),
        }
    }

    async fn send_request(
        &mut self,
        request: HttpRequest<StreamingBody>,
    ) -> Result<Response<Incoming>, LocalHttpError> {
        match self {
            Self::V1(sender) => {
                let mut hyper_request: Request<_> = request.internal_request.into();
                downgrade_to_http1(&mut hyper_request);

                // Solves a "connection was not ready" client error.
                // https://rust-lang.github.io/wg-async/vision/submitted_stories/status_quo/barbara_tries_unix_socket.html#the-single-magical-line
                sender.ready().await.map_err(LocalHttpError::SendFailed)?;

                sender
                    .send_request(hyper_request)
                    .await
                    .map_err(LocalHttpError::SendFailed)
            }
            Self::V2(sender) => {
                let mut hyper_request: Request<_> = request.internal_request.into();

                // fixes https://github.com/metalbear-co/mirrord/issues/2497
                // inspired by https://github.com/linkerd/linkerd2-proxy/blob/c5d9f1c1e7b7dddd9d75c0d1a0dca68188f38f34/linkerd/proxy/http/src/h2.rs#L175
                if hyper_request.uri().authority().is_none()
                    && hyper_request.version() != Version::HTTP_11
                {
                    tracing::trace!(
                        original_version = ?hyper_request.version(),
                        "Request URI has no authority, changing HTTP version to {:?}",
                        Version::HTTP_11,
                    );

                    *hyper_request.version_mut() = Version::HTTP_11;
                }

                // Solves a "connection was not ready" client error.
                // https://rust-lang.github.io/wg-async/vision/submitted_stories/status_quo/barbara_tries_unix_socket.html#the-single-magical-line
                sender.ready().await.map_err(LocalHttpError::SendFailed)?;

                sender
                    .send_request(hyper_request)
                    .await
                    .map_err(LocalHttpError::SendFailed)
            }
        }
    }
}

#[cfg(test)]
mod test {
    use std::ops::Not;

    use hyper::{Method, Request, Uri, Version, header::HOST};
    use mirrord_protocol::tcp::{HttpRequest, InternalHttpRequest};
    use rstest::rstest;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
    };

    use super::{HttpSender, StreamingBody, downgrade_to_http1};

    /// Builds a request as [`hyper`]'s HTTP/2 server produces it: version [`Version::HTTP_2`], the
    /// target in absolute form, and no `Host` header.
    fn http2_request(uri: &str) -> Request<()> {
        let mut request = Request::new(());
        *request.uri_mut() = uri.parse::<Uri>().unwrap();
        *request.version_mut() = Version::HTTP_2;
        request
    }

    /// Verifies that a request converted to HTTP/1 carries a `Host` header made from the
    /// authority, and that the target is rewritten to origin form.
    #[rstest]
    #[case::with_path("http://some.server.com/api/v1?q=1", "some.server.com", "/api/v1?q=1")]
    #[case::with_port("http://some.server.com:8080/", "some.server.com:8080", "/")]
    #[case::empty_path("http://some.server.com", "some.server.com", "/")]
    #[case::userinfo_stripped("http://user@some.server.com/", "some.server.com", "/")]
    #[test]
    fn downgrade_sets_host_and_origin_form(
        #[case] uri: &str,
        #[case] expected_host: &str,
        #[case] expected_target: &str,
    ) {
        let mut request = http2_request(uri);
        downgrade_to_http1(&mut request);

        assert_eq!(request.headers().get(HOST).unwrap(), expected_host);
        assert_eq!(request.uri().to_string(), expected_target);
        assert_eq!(request.version(), Version::HTTP_11);
    }

    /// Verifies that a `Host` header the original request carried is not replaced.
    ///
    /// An HTTP/2 request may carry both, and [RFC 9113 section 8.3.1] states that the authority is
    /// used only when there is no `Host` header.
    ///
    /// [RFC 9113 section 8.3.1]: https://www.rfc-editor.org/rfc/rfc9113#section-8.3.1
    #[test]
    fn downgrade_keeps_original_host_header() {
        let mut request = http2_request("http://from.authority.com/");
        request
            .headers_mut()
            .insert(HOST, "from.header.com".parse().unwrap());

        downgrade_to_http1(&mut request);

        assert_eq!(request.headers().get(HOST).unwrap(), "from.header.com");
    }

    /// Verifies that a request that did not come from an HTTP/2 connection is left alone.
    ///
    /// An HTTP/1 client is allowed to send a target in absolute form, and it sends its own `Host`
    /// header. Neither is ours to rewrite.
    #[test]
    fn downgrade_does_not_touch_http1_requests() {
        let mut request = http2_request("http://some.server.com/api/v1");
        *request.version_mut() = Version::HTTP_11;
        request
            .headers_mut()
            .insert(HOST, "other.server.com".parse().unwrap());

        downgrade_to_http1(&mut request);

        assert_eq!(request.uri().to_string(), "http://some.server.com/api/v1");
        assert_eq!(request.headers().get(HOST).unwrap(), "other.server.com");
    }

    /// Verifies that the target of a CONNECT request is not rewritten.
    ///
    /// The authority of such a request is the target itself, and there is no path to replace it
    /// with.
    #[test]
    fn downgrade_does_not_touch_connect_target() {
        let mut request = http2_request("some.server.com:443");
        *request.method_mut() = Method::CONNECT;

        downgrade_to_http1(&mut request);

        assert_eq!(request.uri().to_string(), "some.server.com:443");
        assert!(request.headers().get(HOST).is_none());
    }

    /// Verifies end-to-end that a stolen HTTP/2 request reaches the local application's HTTP/1
    /// server with a `Host` header.
    ///
    /// Servers that enforce the HTTP/1.1 `Host` requirement reject a request without it before the
    /// application is involved, so this is checked on the bytes that go out on the wire.
    #[tokio::test]
    async fn sends_host_header_to_local_http1_server() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut connection, _) = listener.accept().await.unwrap();

            let mut head = Vec::new();
            while head.windows(4).any(|window| window == b"\r\n\r\n").not() {
                let read = connection.read_buf(&mut head).await.unwrap();
                assert_ne!(read, 0, "the client closed the connection");
            }

            connection
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
                .await
                .unwrap();

            String::from_utf8(head).unwrap()
        });

        let stream = TcpStream::connect(addr).await.unwrap();
        let mut sender = HttpSender::handshake(Version::HTTP_11, stream)
            .await
            .unwrap();
        let request = HttpRequest {
            connection_id: 0,
            request_id: 0,
            port: addr.port(),
            internal_request: InternalHttpRequest {
                method: Method::GET,
                uri: "http://some.server.com:8080/api/v1?q=1".parse().unwrap(),
                headers: Default::default(),
                version: Version::HTTP_2,
                body: StreamingBody::default(),
            },
        };

        let response = sender.send_request(request).await.unwrap();
        assert_eq!(response.status(), 200);

        let head = server.await.unwrap();
        let (request_line, headers) = head.split_once("\r\n").unwrap();
        assert_eq!(request_line, "GET /api/v1?q=1 HTTP/1.1");
        assert!(
            headers
                .lines()
                .any(|line| line.eq_ignore_ascii_case("host: some.server.com:8080")),
            "request head is missing the host header:\n{head}",
        );
    }
}
