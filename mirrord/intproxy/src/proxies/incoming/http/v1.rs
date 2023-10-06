//! [`HttpV1`]
//!
//! Handles HTTP/1 requests.
use std::{convert::Infallible, future};

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use hyper::client::conn::http1::{self, Connection, SendRequest};
use hyper_util::rt::TokioIo;
use mirrord_protocol::tcp::HttpRequestFallback;
use tokio::net::TcpStream;

use super::HttpV;

/// Handles HTTP/1 requests.
///
/// Sends the request to `destination`, and gets back a response.
///
/// See [`HttpInterceptor`](crate::proxies::incoming::http_interceptor::HttpInterceptor) for usage.
pub struct HttpV1(http1::SendRequest<BoxBody<Bytes, Infallible>>);

impl HttpV for HttpV1 {
    type Sender = SendRequest<BoxBody<Bytes, Infallible>>;

    type Connection = Connection<TokioIo<TcpStream>, BoxBody<Bytes, Infallible>>;

    fn new(http_request_sender: Self::Sender) -> Self {
        Self(http_request_sender)
    }

    #[tracing::instrument(level = "trace")]
    async fn handshake(target_stream: TcpStream) -> hyper::Result<Self::Sender> {
        let (sender, connection) = http1::handshake(TokioIo::new(target_stream)).await?;
        tokio::spawn(connection);
        Ok(sender)
    }

    #[tracing::instrument(level = "trace", skip(self))]
    async fn send_request(
        &mut self,
        request: HttpRequestFallback,
    ) -> hyper::Result<hyper::Response<hyper::body::Incoming>> {
        let request_sender = &mut self.0;

        // Solves a "connection was not ready" client error.
        // https://rust-lang.github.io/wg-async/vision/submitted_stories/status_quo/barbara_tries_unix_socket.html#the-single-magical-line
        future::poll_fn(|cx| request_sender.poll_ready(cx)).await?;

        request_sender.send_request(request.into_hyper()).await
    }
}
