use std::future::Future;

use bytes::Bytes;
use http_body_util::Full;
use hyper::{
    client::conn::http2::{self, Connection, SendRequest},
    rt::Executor,
};
use mirrord_protocol::tcp::HttpRequest;
use tokio::net::TcpStream;
use tracing::trace;

use super::HttpVersionT;
use crate::{detour::DetourGuard, tcp_steal::http_forwarding::HttpForwarderError};

// TODO(alex): Import this from `hyper-util` when the crate is actually published.
/// Future executor that utilises `tokio` threads.
#[non_exhaustive]
#[derive(Default, Debug, Clone)]
pub struct TokioExecutor;

impl<Fut> Executor<Fut> for TokioExecutor
where
    Fut: Future + Send + 'static,
    Fut::Output: Send + 'static,
{
    fn execute(&self, fut: Fut) {
        trace!("starting tokio executor for hyper HTTP/2");
        tokio::spawn(async move {
            let _ = DetourGuard::new();
            fut.await
        });
    }
}

/// Handles HTTP/2 requests.
///
/// Sends the request to `destination`, and gets back a response.
///
/// See [`ConnectionTask`] for usage.
pub(crate) struct HttpV2(http2::SendRequest<Full<Bytes>>);

impl HttpVersionT for HttpV2 {
    type Sender = SendRequest<Full<Bytes>>;

    type Connection = Connection<TcpStream, Full<Bytes>>;

    fn new(http_request_sender: Self::Sender) -> Self {
        Self(http_request_sender)
    }

    async fn handshake(
        target_stream: TcpStream,
    ) -> Result<(Self::Sender, Self::Connection), HttpForwarderError> {
        Ok(http2::handshake(TokioExecutor::default(), target_stream).await?)
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
