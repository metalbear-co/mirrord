use std::{error::Error, fmt, io, net::SocketAddr, ops::Not, time::Duration};

use bytes::Bytes;
use exponential_backoff::Backoff;
use hyper::{
    body::{Frame, Incoming},
    client::conn::{http1, http2},
    Request, Response, StatusCode, Version,
};
use hyper_util::rt::{TokioExecutor, TokioIo};
use mirrord_protocol::{
    batched_body::BatchedBody,
    tcp::{HttpRequest, HttpResponse, InternalHttpResponse},
    ConnectionId, Port, RequestId,
};
use thiserror::Error;
use tokio::{net::TcpStream, time};
use tracing::Level;

use super::{bound_socket::BoundTcpSocket, streaming_body::StreamingBody};

/// A retrying HTTP client used to pass requests to the user application.
pub struct LocalHttpClient {
    /// Established HTTP connection with the user application.
    sender: Option<HttpSender>,
    /// Established TCP connection with the user application.
    stream: Option<TcpStream>,
    /// Address of the user application's HTTP server.
    local_server_address: SocketAddr,
}

impl LocalHttpClient {
    /// How many times we attempt to send any given request.
    ///
    /// See [`LocalHttpError::can_retry`].
    const MAX_SEND_ATTEMPTS: u32 = 10;
    const MIN_SEND_BACKOFF: Duration = Duration::from_millis(10);
    const MAX_SEND_BACKOFF: Duration = Duration::from_millis(250);

    /// Crates a new client that will initially use the given `stream` (connection with the user
    /// application's HTTP server).
    pub fn new_for_stream(stream: TcpStream) -> Result<Self, LocalHttpError> {
        let local_server_address = stream
            .peer_addr()
            .map_err(LocalHttpError::SocketSetupFailed)?;

        Ok(Self {
            sender: None,
            stream: Some(stream),
            local_server_address,
        })
    }

    /// Reuses or creates a new [`HttpSender`].
    #[tracing::instrument(level = Level::TRACE, err(level = Level::TRACE))]
    async fn get_sender(&mut self, version: Version) -> Result<HttpSender, LocalHttpError> {
        if let Some(sender) = self.sender.take() {
            if sender.version_matches(version) {
                return Ok(sender);
            }
        }

        let stream = match self.stream.take() {
            Some(stream) => stream,
            None => {
                let socket =
                    BoundTcpSocket::bind_specified_or_localhost(self.local_server_address.ip())
                        .map_err(LocalHttpError::SocketSetupFailed)?;
                socket
                    .connect(self.local_server_address)
                    .await
                    .map_err(LocalHttpError::ConnectTcpFailed)?
            }
        };

        HttpSender::handshake(version, stream).await
    }

    /// Tries to send the given `request` to the user application's HTTP server.
    ///
    /// Checks whether some reponse [`Frame`]s are instantly available.
    async fn try_send_request(
        &mut self,
        request: &HttpRequest<StreamingBody>,
    ) -> Result<Response<PeekedBody>, LocalHttpError> {
        let mut sender = self.get_sender(request.version()).await?;
        let response = sender.send_request(request.clone()).await?;
        let (parts, mut body) = response.into_parts();

        let frames = body
            .ready_frames()
            .map_err(LocalHttpError::ReadBodyFailed)?;
        let body = PeekedBody {
            head: frames.frames,
            tail: frames.is_last.not().then_some(body),
        };

        self.sender.replace(sender);

        Ok(Response::from_parts(parts, body))
    }

    /// Tries to send the given `request` to the user application's HTTP server.
    ///
    /// Retries on known errors (see [`LocalHttpError::can_retry`]).
    #[tracing::instrument(level = Level::DEBUG, err(level = Level::WARN), ret)]
    pub async fn send_request(
        &mut self,
        request: &HttpRequest<StreamingBody>,
    ) -> Result<Response<PeekedBody>, LocalHttpError> {
        let mut backoffs = Backoff::new(
            Self::MAX_SEND_ATTEMPTS,
            Self::MIN_SEND_BACKOFF,
            Self::MAX_SEND_BACKOFF,
        )
        .into_iter()
        .flatten();

        let mut attempt = 0;
        loop {
            attempt += 1;
            tracing::trace!(attempt, "Trying to send the request");
            match (self.try_send_request(&request).await, backoffs.next()) {
                (Ok(response), _) => {
                    tracing::trace!(
                        attempt,
                        "Successfully sent the request and peeked first frames"
                    );
                    break Ok(response);
                }

                (Err(error), Some(backoff)) if error.can_retry() => {
                    tracing::warn!(
                        attempt,
                        connection_id = request.connection_id,
                        request_id = request.request_id,
                        %error,
                        backoff_s = backoff.as_secs_f32(),
                        "Failed to send the request to the local application, retrying",
                    );

                    time::sleep(backoff).await;
                }

                (Err(error), _) => {
                    tracing::warn!(
                        attempts = attempt,
                        connection_id = request.connection_id,
                        request_id = request.request_id,
                        %error,
                        "Failed to send the request to the local application",
                    );

                    break Err(error);
                }
            }
        }
    }
}

impl fmt::Debug for LocalHttpClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalHttpClient")
            .field("has_sender", &self.sender.is_some())
            .field("has_stream", &self.stream.is_some())
            .field("local_server_address", &self.local_server_address)
            .finish()
    }
}

/// Errors that can occur when sending an HTTP request to the user application.
#[derive(Error, Debug)]
pub enum LocalHttpError {
    #[error("HTTP handshake failed: {0}")]
    HandshakeFailed(#[source] hyper::Error),

    #[error("{0:?} is not supported")]
    UnsupportedHttpVersion(Version),

    #[error("sending the request failed: {0}")]
    SendFailed(#[source] hyper::Error),

    #[error("setting up TCP socket failed: {0}")]
    SocketSetupFailed(#[source] io::Error),

    #[error("making a TPC connection failed: {0}")]
    ConnectTcpFailed(#[source] io::Error),

    #[error("reading the response body failed: {0}")]
    ReadBodyFailed(#[source] hyper::Error),
}

impl LocalHttpError {
    /// Checks if the given [`hyper::Error`] originates from [`h2::Error`] `RST_STREAM`.
    ///
    /// This requires that we use the same [`h2`] version as [`hyper`],
    /// which is verified in the `hyper_and_h2_versions_in_sync` test below.
    pub fn is_h2_reset(error: &hyper::Error) -> bool {
        let mut cause = error.source();
        while let Some(err) = cause {
            if let Some(typed) = err.downcast_ref::<h2::Error>() {
                return typed.is_reset();
            };

            cause = err.source();
        }

        false
    }

    /// Checks if we can retry sending the request, given that the previous attempt resulted in this
    /// error.
    pub fn can_retry(&self) -> bool {
        match self {
            Self::SocketSetupFailed(..) | Self::UnsupportedHttpVersion(..) => false,
            Self::ConnectTcpFailed(..) => true,
            Self::HandshakeFailed(err) | Self::SendFailed(err) | Self::ReadBodyFailed(err) => {
                err.is_closed() || err.is_incomplete_message() || Self::is_h2_reset(err)
            }
        }
    }

    /// Produces a [`StatusCode::BAD_GATEWAY`] response from this error.
    pub fn as_error_response(
        &self,
        version: Version,
        request_id: RequestId,
        connection_id: ConnectionId,
        port: Port,
    ) -> HttpResponse<Vec<u8>> {
        HttpResponse {
            request_id,
            connection_id,
            port,
            internal_response: InternalHttpResponse {
                status: StatusCode::BAD_GATEWAY,
                version,
                headers: Default::default(),
                body: format!("mirrord: {self}").into_bytes(),
            },
        }
    }
}

/// Response body returned from [`LocalHttpClient`].
pub struct PeekedBody {
    /// [`Frame`]s that were instantly available.
    pub head: Vec<Frame<Bytes>>,
    /// The rest of the response's body.
    pub tail: Option<Incoming>,
}

impl fmt::Debug for PeekedBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PeekedBody")
            .field("head", &self.head)
            .field("has_tail", &self.tail.is_some())
            .finish()
    }
}

/// Holds either [`http1::SendRequest`] or [`http2::SendRequest`] and exposes a unified interface.
enum HttpSender {
    V1(http1::SendRequest<StreamingBody>),
    V2(http2::SendRequest<StreamingBody>),
}

impl HttpSender {
    /// Performs an HTTP handshake over the given [`TcpStream`].
    #[tracing::instrument(level = Level::DEBUG, skip(target_stream), err(level = Level::WARN))]
    async fn handshake(version: Version, target_stream: TcpStream) -> Result<Self, LocalHttpError> {
        let local_addr = target_stream
            .local_addr()
            .map_err(LocalHttpError::SocketSetupFailed)?;
        let peer_addr = target_stream
            .peer_addr()
            .map_err(LocalHttpError::SocketSetupFailed)?;

        match version {
            Version::HTTP_2 => {
                let (sender, connection) =
                    http2::handshake(TokioExecutor::default(), TokioIo::new(target_stream))
                        .await
                        .map_err(LocalHttpError::HandshakeFailed)?;

                tokio::spawn(async move {
                    match connection.await {
                        Ok(()) => {
                            tracing::trace!(%local_addr, %peer_addr, "HTTP connection with the local application finished");
                        }
                        Err(error) => {
                            tracing::warn!(%error, %local_addr, %peer_addr, "HTTP connection with the local application failed");
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
                            tracing::trace!(%local_addr, %peer_addr, "HTTP connection with the local application finished");
                        }
                        Err(error) => {
                            tracing::warn!(%error, %local_addr, %peer_addr, "HTTP connection with the local application failed");
                        }
                    }
                });

                Ok(HttpSender::V1(sender))
            }
        }
    }

    /// Tries to send the given [`HttpRequest`] to the server.
    #[tracing::instrument(
        level = Level::DEBUG,
        skip(self, request),
        fields(connection_id = request.connection_id, request_id = request.request_id),
        ret,
        err(level = Level::WARN),
    )]
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

    /// Returns whether this [`HttpSender`] can handle requests of the given [`Version`].
    fn version_matches(&self, version: Version) -> bool {
        match (version, self) {
            (Version::HTTP_2, Self::V2(..)) => true,
            (Version::HTTP_3, _) => false,
            (_, Self::V1(..)) => true,
            _ => false,
        }
    }
}

#[cfg(test)]
mod test {
    use std::{
        convert::Infallible,
        error::Error,
        net::{Ipv4Addr, SocketAddr},
    };

    use bytes::Bytes;
    use http_body_util::Full;
    use hyper::{server::conn::http2, service::service_fn, Response};
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use tokio::net::TcpListener;

    /// Checks that [`hyper`] and [`h2`] crate versions are in sync with each other.
    ///
    /// In [`LocalHttpError::is_h2_reset`](super::LocalHttpError::is_h2_reset) we use
    /// `source.downcast_ref::<h2::Error>` to drill down on [`h2`] errors from [`hyper`], we
    /// need these two crates to stay in sync, otherwise we could always fail some of our checks
    /// that rely on this `downcast` working.
    ///
    /// Even though we're using [`h2::Error::is_reset`] in intproxy, this test can be
    /// for any error, and thus here we do it for [`h2::Error::is_go_away`] which is
    /// easier to trigger.
    #[tokio::test]
    async fn hyper_and_h2_versions_in_sync() {
        let listener = TcpListener::bind(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0))
            .await
            .unwrap();
        let listener_address = listener.local_addr().unwrap();

        let handle = tokio::spawn(async move {
            let stream = listener.accept().await.unwrap().0;
            http2::Builder::new(TokioExecutor::default())
                .serve_connection(
                    TokioIo::new(stream),
                    service_fn(|_| async move {
                        Ok::<_, Infallible>(Response::new(Full::new(Bytes::from("Heresy!"))))
                    }),
                )
                .await
        });

        assert!(reqwest::get(format!("https://{listener_address}"))
            .await
            .is_err());

        let conn_result = handle.await.unwrap();
        assert!(
            conn_result
                .as_ref()
                .err()
                .and_then(Error::source)
                .and_then(|source| source.downcast_ref::<h2::Error>())
                .is_some_and(h2::Error::is_go_away),
            r"The request is supposed to fail with `GO_AWAY`! 
            Something is wrong if it didn't!

            >> If you're seeing this error, the cause is likely that `hyper` and `h2`
            versions are out of sync, and we can't have that due to our use of
            `downcast_ref` on some `h2` errors!"
        );
    }
}
