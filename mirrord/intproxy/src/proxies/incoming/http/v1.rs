//! # [`HttpV1Connector`]
//!
//! Handles HTTP/1 requests.

use std::convert::Infallible;

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;
use mirrord_protocol::tcp::HttpRequestFallback;
use tokio::net::TcpStream;

use super::HttpConnector;

/// Implementation of [`HttpConnector`] for HTTP/1
pub struct HttpV1Connector(http1::SendRequest<BoxBody<Bytes, Infallible>>);

impl HttpConnector for HttpV1Connector {
    #[tracing::instrument(level = "trace")]
    async fn handshake(target_stream: TcpStream) -> hyper::Result<Self> {
        let (sender, connection) = http1::handshake(TokioIo::new(target_stream)).await?;
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
