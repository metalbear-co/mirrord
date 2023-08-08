//! # [`HttpV2`]
//!
//! Handles HTTP/2 requests.
use std::{
    convert::Infallible,
    future::{self, Future},
};

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use hyper::{
    client::conn::http2::{self, Connection, SendRequest},
    rt::Executor,
};
use hyper_util::rt::{TokioExecutor, TokioIo};
use mirrord_protocol::tcp::HttpRequestFallback;
use tokio::net::TcpStream;
use tracing::trace;

use super::HttpV;
use crate::{detour::DetourGuard, tcp_steal::http_forwarding::HttpForwarderError};

/// Thin wrapper over [`TokioExecutor`].
/// Makes sure that detours are bypassed when executing futures.
#[non_exhaustive]
#[derive(Default, Debug, Clone)]
pub struct DetourGuardExecutor {
    inner: TokioExecutor,
}

impl<Fut> Executor<Fut> for DetourGuardExecutor
where
    Fut: Future + Send + 'static,
    Fut::Output: Send + 'static,
{
    fn execute(&self, fut: Fut) {
        trace!("starting tokio executor for hyper HTTP/2");

        self.inner.execute(async move {
            let _guard = DetourGuard::new();
            fut.await
        });
    }
}

/// Handles HTTP/2 requests.
///
/// Sends the request to `destination`, and gets back a response.
///
/// See [`ConnectionTask`] for usage.
pub(crate) struct HttpV2(http2::SendRequest<BoxBody<Bytes, Infallible>>);

impl HttpV for HttpV2 {
    type Sender = SendRequest<BoxBody<Bytes, Infallible>>;

    type Connection =
        Connection<TokioIo<TcpStream>, BoxBody<Bytes, Infallible>, DetourGuardExecutor>;

    #[tracing::instrument(level = "trace")]
    fn new(http_request_sender: Self::Sender) -> Self {
        Self(http_request_sender)
    }

    #[tracing::instrument(level = "trace")]
    async fn handshake(
        target_stream: TcpStream,
    ) -> Result<(Self::Sender, Self::Connection), HttpForwarderError> {
        Ok(http2::handshake(DetourGuardExecutor::default(), TokioIo::new(target_stream)).await?)
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

    fn take_sender(self) -> Self::Sender {
        self.0
    }

    fn set_sender(&mut self, new_sender: Self::Sender) {
        self.0 = new_sender;
    }
}
