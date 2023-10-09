use std::future::Future;

use hyper::{body::Incoming, Response};
use mirrord_protocol::tcp::HttpRequestFallback;
use tokio::net::TcpStream;

pub mod v1;
pub mod v2;

/// Handles the differences between hyper's HTTP/1 and HTTP/2 connections.
///
/// Thanks to this trait being implemented for both [`v1::HttpV1Connector`] and
/// [`v2::HttpV2Connector`], we can have most of the implementation for dealing with requests in
/// [`HttpInterceptor`](super::http_interceptor::HttpInterceptor).
pub trait HttpConnector: Sized {
    /// Calls the appropriate `HTTP/V` `handshake` method and returns a new connector.
    fn handshake(target_stream: TcpStream) -> impl Future<Output = hyper::Result<Self>> + Send;

    /// Sends the [`HttpRequestFallback`] to its destination and returns the response.
    fn send_request(
        &mut self,
        request: HttpRequestFallback,
    ) -> impl Future<Output = hyper::Result<Response<Incoming>>> + Send;
}
