use std::future::Future;

use hyper::{body::Incoming, Response};
use mirrord_protocol::tcp::HttpRequestFallback;
use tokio::net::TcpStream;

pub mod v1;
pub mod v2;

/// Handles the differences between hyper's HTTP/1 and HTTP/2 connections.
///
/// Thanks to this trait being implemented for both [`v1::HttpV1`] and [`v2::HttpV2`], we can have
/// most of the implementation for dealing with requests in [`HttpInterceptor`].
pub trait HttpV: Sized {
    type Sender;
    type Connection: Future + Send + 'static;

    /// Creates a new [`ConnectionTask`] that handles [`v1::HttpV1`]/[`v2::HttpV2`] requests.
    ///
    /// Connects to the user's application with `Self::connect_to_application`.
    fn new(http_request_sender: Self::Sender) -> Self;

    /// Calls the appropriate `HTTP/V` `handshake` method.
    fn handshake(
        target_stream: TcpStream,
    ) -> impl Future<Output = hyper::Result<Self::Sender>> + Send;

    /// Sends the [`HttpRequest`] to its destination.
    ///
    /// Required due to this mechanism having distinct types;
    /// HTTP/1 with [`http1::SendRequest`], and HTTP/2 with [`http2::SendRequest`].
    ///
    /// [`http1::SendRequest`]: hyper::client::conn::http1::SendRequest
    /// [`http2::SendRequest`]: hyper::client::conn::http2::SendRequest
    fn send_request(
        &mut self,
        request: HttpRequestFallback,
    ) -> impl Future<Output = hyper::Result<Response<Incoming>>> + Send;
}
