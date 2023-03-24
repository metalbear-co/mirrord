use std::{future::Future, net::SocketAddr};

use bytes::Bytes;
use futures::FutureExt;
use http_body_util::Full;
use hyper::{
    client::conn::http2::{self, Connection, SendRequest},
    rt::Executor,
};
use mirrord_protocol::{
    tcp::{HttpRequest, HttpResponse},
    ConnectionId, Port,
};
use tokio::net::TcpStream;
use tracing::{error, trace};

use super::{handle_response, ConnectionTask, HttpVersionT};
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

impl HttpV2 {
    /// Sends the [`HttpRequest`] through `Self::sender`, converting the response into a
    /// [`HttpResponse`].
    async fn send_http_request_to_application(
        &mut self,
        request: HttpRequest,
        port: Port,
        connection_id: ConnectionId,
    ) -> Result<HttpResponse, HttpForwarderError> {
        let request_id = request.request_id;

        let response = self
            .sender
            .send_request(request.internal_request.clone().into())
            .map(|response| handle_response(request, response, port, connection_id, request_id))
            .await
            .await;

        // Retry once if the connection was closed.
        if let Err(HttpForwarderError::ConnectionClosedTooSoon(request)) = response {
            let Self {
                destination: address,
                sender,
            } = ConnectionTask::<Self>::connect_to_application(self.destination).await?;

            self.destination = address;
            self.sender = sender;

            Ok(self
                .sender
                .send_request(request.internal_request.clone().into())
                .map(|response| handle_response(request, response, port, connection_id, request_id))
                .await
                .await?)
        } else {
            response
        }
    }
}

impl ConnectionTask<HttpV2> {
    /// Creates a new [`ConnectionTask`] that handles [`HttpV1`] requests.
    ///
    /// Connects to the user's application with `Self::connect_to_application`.
    /// Creates a client HTTP/2 [`http2::Connection`] to the user's application.
    ///
    /// Requests that match the user specified filter will be sent through this connection to the
    /// user.
    #[tracing::instrument(level = "trace")]
    async fn connect_to_application(connect_to: SocketAddr) -> Result<HttpV2, HttpForwarderError> {
        let http_request_sender = {
            let _ = DetourGuard::new();
            let target_stream = TcpStream::connect(connect_to).await?;

            let (http_request_sender, connection) = http2::Builder::new()
                .executor(TokioExecutor::default())
                .handshake(target_stream)
                .await?;

            // spawn a task to poll the connection.
            tokio::spawn(async move {
                if let Err(fail) = connection.await {
                    error!("Error in http connection with addr {connect_to:?}: {fail:?}");
                }
            });

            http_request_sender
        };

        Ok(HttpV2 {
            destination: connect_to,
            sender: http_request_sender,
        })
    }

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
}
