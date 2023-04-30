//! [`HttpV1`]
//!
//! Handles HTTP/1 requests.
use std::future;

use bytes::Bytes;
use http_body_util::Full;
use hyper::client::conn::http1::{self, Connection, SendRequest};
use mirrord_protocol::tcp::HttpRequest;
use tokio::net::TcpStream;

use super::HttpV;
use crate::tcp_steal::http_forwarding::HttpForwarderError;

/// Handles HTTP/1 requests.
///
/// Sends the request to `destination`, and gets back a response.
///
/// See [`ConnectionTask`] for usage.
pub(crate) struct HttpV1(http1::SendRequest<Full<Bytes>>);

impl HttpV for HttpV1 {
    type Sender = SendRequest<Full<Bytes>>;

    type Connection = Connection<TcpStream, Full<Bytes>>;

    #[tracing::instrument(level = "trace")]
    fn new(http_request_sender: Self::Sender) -> Self {
        Self(http_request_sender)
    }

    #[tracing::instrument(level = "trace")]
    async fn handshake(
        target_stream: TcpStream,
    ) -> Result<(Self::Sender, Self::Connection), HttpForwarderError> {
        Ok(http1::handshake(target_stream).await?)
    }

    #[tracing::instrument(level = "trace", skip(self))]
    async fn send_request(
        &mut self,
        request: HttpRequest,
    ) -> hyper::Result<hyper::Response<hyper::body::Incoming>> {
        let request_sender = &mut self.0;

        // Solves a "connection was not ready" client error.
        // https://rust-lang.github.io/wg-async/vision/submitted_stories/status_quo/barbara_tries_unix_socket.html#the-single-magical-line
        future::poll_fn(|cx| request_sender.poll_ready(cx)).await?;

        request_sender
            .send_request(request.internal_request.into())
            .await
    }

    fn take_sender(self) -> Self::Sender {
        self.0
    }

    fn set_sender(&mut self, new_sender: Self::Sender) {
        self.0 = new_sender;
    }
}
