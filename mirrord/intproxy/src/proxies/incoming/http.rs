use std::{fmt, io, net::SocketAddr, ops::Not};

use hyper::{
    body::Incoming,
    client::conn::{http1, http2},
    Request, Response, StatusCode, Version,
};
use hyper_util::rt::{TokioExecutor, TokioIo};
use mirrord_protocol::{
    tcp::{HttpRequest, HttpResponse, InternalHttpResponse},
    ConnectionId, Port, RequestId,
};
use rustls::pki_types::{InvalidDnsNameError, ServerName};
use thiserror::Error;
use tls_connector::LazyConnectorError;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpStream,
};
use tokio_rustls::TlsConnector;
use tracing::Level;

mod client_store;
mod response_mode;
mod streaming_body;
mod tls_connector;

pub use client_store::ClientStore;
pub use response_mode::ResponseMode;
pub use streaming_body::StreamingBody;

/// An HTTP client used to pass requests to the user application.
pub struct LocalHttpClient {
    /// Established HTTP connection with the user application.
    sender: HttpSender,
    /// Server name we used when making a TLS connection (for HTTPS).
    ///
    /// If [`None`], this client uses a plain TCP connection (HTTP).
    tls_server_name: Option<ServerName<'static>>,
    /// Address of the user application's HTTP server.
    local_server_address: SocketAddr,
    /// Address of this client's TCP socket.
    address: SocketAddr,
}

impl LocalHttpClient {
    /// Makes an HTTP connection with the given server and creates a new client.
    #[tracing::instrument(
        level = Level::TRACE,
        err(level = Level::WARN),
        ret,
    )]
    pub async fn new_plain(
        local_server_address: SocketAddr,
        version: Version,
    ) -> Result<Self, LocalHttpError> {
        let stream = TcpStream::connect(local_server_address)
            .await
            .map_err(LocalHttpError::ConnectTcpFailed)?;
        let local_server_address = stream
            .peer_addr()
            .map_err(LocalHttpError::SocketSetupFailed)?;
        let address = stream
            .local_addr()
            .map_err(LocalHttpError::SocketSetupFailed)?;
        let sender = HttpSender::handshake(version, stream, address, local_server_address).await?;

        Ok(Self {
            sender,
            tls_server_name: None,
            local_server_address,
            address,
        })
    }

    /// Makes an HTTPS connection with the given server and creates a new client.
    #[tracing::instrument(
        level = Level::TRACE,
        skip(connector),
        err(level = Level::WARN),
        ret,
    )]
    pub async fn new_tls(
        local_server_address: SocketAddr,
        version: Version,
        server_name: ServerName<'static>,
        connector: &TlsConnector,
    ) -> Result<Self, LocalHttpError> {
        let stream = TcpStream::connect(local_server_address)
            .await
            .map_err(LocalHttpError::ConnectTcpFailed)?;
        let stream = connector
            .connect(server_name, stream)
            .await
            .map_err(LocalHttpError::ConnectTlsFailed)?;
        let local_server_address = stream
            .get_ref()
            .0
            .peer_addr()
            .map_err(LocalHttpError::SocketSetupFailed)?;
        let address = stream
            .get_ref()
            .0
            .local_addr()
            .map_err(LocalHttpError::SocketSetupFailed)?;
        let sender = HttpSender::handshake(version, stream, address, local_server_address).await?;

        Ok(Self {
            sender,
            tls_server_name: None,
            local_server_address,
            address,
        })
    }

    /// Send the given `request` to the user application's HTTP server.
    #[tracing::instrument(level = Level::DEBUG, err(level = Level::WARN), ret)]
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

    pub fn tls_server_name(&self) -> Option<&ServerName<'static>> {
        self.tls_server_name.as_ref()
    }
}

impl fmt::Debug for LocalHttpClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalHttpClient")
            .field("local_server_address", &self.local_server_address)
            .field("address", &self.address)
            .field("is_http_1", &matches!(self.sender, HttpSender::V1(..)))
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

    #[error("failed to extract server name from request: {0}")]
    InvalidDnsNameError(#[from] InvalidDnsNameError),

    #[error("failed to build a TLS connector: {0}")]
    LazyConnectorError(#[from] LazyConnectorError),
}

impl LocalHttpError {
    /// Checks if we can retry sending the request, given that the previous attempt resulted in this
    /// error.
    pub fn can_retry(&self) -> bool {
        match self {
            Self::SocketSetupFailed(..)
            | Self::UnsupportedHttpVersion(..)
            | Self::InvalidDnsNameError(..)
            | Self::LazyConnectorError(..) => false,
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
) -> HttpResponse<Vec<u8>> {
    HttpResponse {
        connection_id,
        port,
        request_id,
        internal_response: InternalHttpResponse {
            status: StatusCode::BAD_GATEWAY,
            version,
            headers: Default::default(),
            body: format!("mirrord: {message}\n").into_bytes(),
        },
    }
}

/// Holds either [`http1::SendRequest`] or [`http2::SendRequest`] and exposes a unified interface.
enum HttpSender {
    V1(http1::SendRequest<StreamingBody>),
    V2(http2::SendRequest<StreamingBody>),
}

impl HttpSender {
    /// Performs an HTTP handshake over the given `target_stream`.
    async fn handshake<IO>(
        version: Version,
        target_stream: IO,
        http_client_addr: SocketAddr,
        http_server_addr: SocketAddr,
    ) -> Result<Self, LocalHttpError>
    where
        IO: 'static + AsyncRead + AsyncWrite + Send + Unpin,
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
                            tracing::trace!(%http_client_addr, %http_server_addr, "HTTP connection with the local application finished");
                        }
                        Err(error) => {
                            tracing::warn!(%error, %http_client_addr, %http_server_addr, "HTTP connection with the local application failed");
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
                            tracing::trace!(%http_client_addr, %http_server_addr, "HTTP connection with the local application finished");
                        }
                        Err(error) => {
                            tracing::warn!(%error, %http_client_addr, %http_server_addr, "HTTP connection with the local application failed");
                        }
                    }
                });

                Ok(HttpSender::V1(sender))
            }
        }
    }

    /// Tries to send the given [`HttpRequest`] to the server.
    async fn send_request(
        &mut self,
        request: HttpRequest<StreamingBody>,
    ) -> Result<Response<Incoming>, LocalHttpError> {
        match self {
            Self::V1(sender) => {
                // Solves a "connection was not ready" client error.
                // https://rust-lang.github.io/wg-async/vision/submitted_stories/status_quo/barbara_tries_unix_socket.html#the-single-magical-line
                sender.ready().await.map_err(LocalHttpError::SendFailed)?;

                sender
                    .send_request(request.internal_request.into())
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
