use std::{future::Future, net::SocketAddr};

use bytes::Bytes;
use http_body_util::Full;
use hyper::{
    client::conn::http2::{self, Connection, SendRequest},
    rt::Executor,
};
use mirrord_protocol::tcp::HttpRequest;
use tokio::net::TcpStream;
use tracing::trace;

use super::connection::{ConnectionTask, HttpVersionT};
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
/// See [`ConnectionTask`] for usage.
pub(super) struct HttpV2 {
    /// Address we're connecting to.
    destination: SocketAddr,

    /// Sends the request to `destination`, and gets back a response.
    sender: http2::SendRequest<Full<Bytes>>,
}
impl ConnectionTask<HttpV2> {
    /// Starts the communication handling of `matched request -> user application -> response` by
    /// "listening" on the `request_receiver`.
    #[tracing::instrument(level = "trace", skip(self))]
    pub(super) async fn start(self) -> Result<(), HttpForwarderError> {
        let Self {
            mut request_receiver,
            response_sender,
            port,
            connection_id,
            mut http_version,
        } = self;

        while let Some(request) = request_receiver.recv().await {
            let response = http_version
                .send_http_request_to_application(request, port, connection_id)
                .await?;

            response_sender.send(response).await?;
        }

        Ok(())
    }
}

impl HttpVersionT for HttpV2 {
    type Sender = SendRequest<Full<Bytes>>;

    type Connection = Connection<TcpStream, Full<Bytes>>;

    fn new(connect_to: SocketAddr, http_request_sender: Self::Sender) -> Self {
        Self {
            destination: connect_to,
            sender: http_request_sender,
        }
    }

    async fn handshake(
        target_stream: TcpStream,
    ) -> Result<(Self::Sender, Self::Connection), HttpForwarderError> {
        Ok(http2::handshake(target_stream).await?)
    }

    fn destination(&self) -> SocketAddr {
        self.destination
    }

    async fn send_request(
        &mut self,
        request: HttpRequest,
    ) -> hyper::Result<hyper::Response<hyper::body::Incoming>> {
        self.sender
            .send_request(request.internal_request.into())
            .await
    }

    fn set_destination(&mut self, new_destination: SocketAddr) {
        self.destination = new_destination;
    }

    fn take_sender(self) -> Self::Sender {
        self.sender
    }

    fn set_sender(&mut self, new_sender: Self::Sender) {
        self.sender = new_sender;
    }
}
