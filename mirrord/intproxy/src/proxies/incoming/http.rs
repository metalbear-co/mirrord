use std::convert::Infallible;

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use hyper::{
    body::Incoming,
    client::conn::{http1, http2},
    Response, Version,
};
use hyper_util::rt::{TokioExecutor, TokioIo};
use mirrord_protocol::tcp::HttpRequestFallback;
use tokio::net::TcpStream;
use tracing::Level;

use super::interceptor::{InterceptorError, InterceptorResult};

pub(super) const RETRY_ON_RESET_ATTEMPTS: u32 = 10;

/// Handles the differences between hyper's HTTP/1 and HTTP/2 connections.
pub enum HttpSender {
    V1(http1::SendRequest<BoxBody<Bytes, Infallible>>),
    V2(http2::SendRequest<BoxBody<Bytes, Infallible>>),
}

/// Consumes the given [`TcpStream`] and performs an HTTP handshake, turning it into an HTTP
/// connection.
///
/// # Returns
///
/// [`HttpSender`] that can be used to send HTTP requests to the peer.
#[tracing::instrument(level = Level::TRACE, skip(target_stream), err(level = Level::WARN))]
pub async fn handshake(
    version: Version,
    target_stream: TcpStream,
) -> InterceptorResult<HttpSender> {
    match version {
        Version::HTTP_2 => {
            let (sender, connection) =
                http2::handshake(TokioExecutor::default(), TokioIo::new(target_stream)).await?;
            tokio::spawn(connection);

            Ok(HttpSender::V2(sender))
        }

        Version::HTTP_3 => Err(InterceptorError::UnsupportedHttpVersion(version)),

        _http_v1 => {
            let (sender, connection) = http1::handshake(TokioIo::new(target_stream)).await?;

            tokio::spawn(connection.with_upgrades());

            Ok(HttpSender::V1(sender))
        }
    }
}

impl HttpSender {
    #[tracing::instrument(level = Level::TRACE, skip(self), err(level = Level::WARN))]
    pub async fn send(
        &mut self,
        req: HttpRequestFallback,
    ) -> InterceptorResult<Response<Incoming>, InterceptorError> {
        match self {
            Self::V1(sender) => {
                // Solves a "connection was not ready" client error.
                // https://rust-lang.github.io/wg-async/vision/submitted_stories/status_quo/barbara_tries_unix_socket.html#the-single-magical-line
                sender.ready().await?;
                sender
                    .send_request(req.into_hyper())
                    .await
                    .map_err(Into::into)
            }
            Self::V2(sender) => {
                let mut req = req.into_hyper();
                // fixes https://github.com/metalbear-co/mirrord/issues/2497
                // inspired by https://github.com/linkerd/linkerd2-proxy/blob/c5d9f1c1e7b7dddd9d75c0d1a0dca68188f38f34/linkerd/proxy/http/src/h2.rs#L175
                if req.uri().authority().is_none() {
                    *req.version_mut() = hyper::http::Version::HTTP_11;
                }
                // Solves a "connection was not ready" client error.
                // https://rust-lang.github.io/wg-async/vision/submitted_stories/status_quo/barbara_tries_unix_socket.html#the-single-magical-line
                sender.ready().await?;
                sender.send_request(req).await.map_err(Into::into)
            }
        }
    }
}
