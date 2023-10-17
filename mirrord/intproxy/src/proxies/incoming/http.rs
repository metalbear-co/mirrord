//! # PR NOTE
//! 
//! Copied from old `HttpV` trait and its implementations (`HttpV1` and `HttpV2`).
//! A bit simplified - turned into the [`HttpConnection`] enum after removing unnecessary associated types.

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

use super::http_interceptor::HttpInterceptorError;

/// Handles the differences between hyper's HTTP/1 and HTTP/2 connections.
pub enum HttpConnection {
    V1(http1::SendRequest<BoxBody<Bytes, Infallible>>),
    V2(http2::SendRequest<BoxBody<Bytes, Infallible>>),
}

impl HttpConnection {
    pub async fn handshake(
        version: Version,
        target_stream: TcpStream,
    ) -> Result<Self, HttpInterceptorError> {
        match version {
            Version::HTTP_2 => {
                let (sender, connection) =
                    http2::handshake(TokioExecutor::default(), TokioIo::new(target_stream)).await?;
                tokio::spawn(connection);

                Ok(Self::V2(sender))
            }

            Version::HTTP_3 => Err(HttpInterceptorError::UnsupportedHttpVersion(version)),

            _http_v1 => {
                let (sender, connection) = http1::handshake(TokioIo::new(target_stream)).await?;
                tokio::spawn(connection);

                Ok(Self::V1(sender))
            }
        }
    }

    pub async fn send_request(
        &mut self,
        req: HttpRequestFallback,
    ) -> Result<Response<Incoming>, HttpInterceptorError> {
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
                // Solves a "connection was not ready" client error.
                // https://rust-lang.github.io/wg-async/vision/submitted_stories/status_quo/barbara_tries_unix_socket.html#the-single-magical-line
                sender.ready().await?;
                sender
                    .send_request(req.into_hyper())
                    .await
                    .map_err(Into::into)
            }
        }
    }
}
