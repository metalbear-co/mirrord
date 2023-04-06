//! [`HttpV1`]
//!
//! Handles HTTP/1 requests.
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

    fn new(http_request_sender: Self::Sender) -> Self {
        Self(http_request_sender)
    }

    async fn handshake(
        target_stream: TcpStream,
    ) -> Result<(Self::Sender, Self::Connection), HttpForwarderError> {
        Ok(http1::handshake(target_stream).await?)
    }

    async fn send_request(
        &mut self,
        request: HttpRequest,
    ) -> hyper::Result<hyper::Response<hyper::body::Incoming>> {
        self.0.send_request(request.internal_request.into()).await
    }

    fn take_sender(self) -> Self::Sender {
        self.0
    }

    fn set_sender(&mut self, new_sender: Self::Sender) {
        self.0 = new_sender;
    }
}
