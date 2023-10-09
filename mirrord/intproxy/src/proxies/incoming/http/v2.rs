//! # [`HttpV2Connector`]
//!
//! Handles HTTP/2 requests.

use std::convert::Infallible;

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use hyper::client::conn::http2;
use hyper_util::rt::{TokioExecutor, TokioIo};
use mirrord_protocol::tcp::HttpRequestFallback;
use tokio::net::TcpStream;

use super::HttpConnector;

/// Implementation of [`HttpConnector`] for HTTP/2.
pub struct HttpV2Connector(http2::SendRequest<BoxBody<Bytes, Infallible>>);

impl HttpConnector for HttpV2Connector {
    #[tracing::instrument(level = "trace")]
    async fn handshake(target_stream: TcpStream) -> hyper::Result<Self> {
        let (sender, connection) =
            http2::handshake(TokioExecutor::default(), TokioIo::new(target_stream)).await?;
        tokio::spawn(connection);

        Ok(Self(sender))
    }

    #[tracing::instrument(level = "trace", skip(self))]
    async fn send_request(
        &mut self,
        request: HttpRequestFallback,
    ) -> hyper::Result<hyper::Response<hyper::body::Incoming>> {
        // Solves a "connection was not ready" client error.
        // https://rust-lang.github.io/wg-async/vision/submitted_stories/status_quo/barbara_tries_unix_socket.html#the-single-magical-line
        self.0.ready().await?;
        self.0.send_request(request.into_hyper()).await
    }
}
