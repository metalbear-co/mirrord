use std::net::SocketAddr;

use bytes::Bytes;
use futures::FutureExt;
use http_body_util::Full;
use hyper::client::conn::http1;
use mirrord_protocol::{
    tcp::{HttpRequest, HttpResponse},
    ConnectionId, Port,
};
use tokio::{
    net::TcpStream,
    sync::mpsc::{Receiver, Sender},
};
use tracing::error;

use super::{handle_response, ConnectionTask};
use crate::{detour::DetourGuard, tcp_steal::http_forwarding::HttpForwarderError};

/// Handles HTTP/1 requests.
///
/// See [`ConnectionTask`] for usage.
pub(super) struct HttpV1 {
    /// Address we're connecting to.
    destination: SocketAddr,

    /// Sends the request to `destination`, and gets back a response.
    sender: http1::SendRequest<Full<Bytes>>,
}

impl HttpV1 {
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
                destination,
                sender,
            } = ConnectionTask::<Self>::connect_to_application(self.destination).await?;

            self.destination = destination;
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

impl ConnectionTask<HttpV1> {
    /// Creates a new [`ConnectionTask`] that handles [`HttpV1`] requests.
    ///
    /// Connects to the user's application with `Self::connect_to_application`.
    pub(super) async fn new(
        connect_to: SocketAddr,
        request_receiver: Receiver<HttpRequest>,
        response_sender: Sender<HttpResponse>,
        port: Port,
        connection_id: ConnectionId,
    ) -> Result<Self, HttpForwarderError> {
        let http_version = Self::connect_to_application(connect_to).await?;

        Ok(Self {
            request_receiver,
            response_sender,
            port,
            connection_id,
            http_version,
        })
    }

    /// Creates a client HTTP/1 [`http1::Connection`] to the user's application.
    ///
    /// Requests that match the user specified filter will be sent through this connection to the
    /// user.
    async fn connect_to_application(connect_to: SocketAddr) -> Result<HttpV1, HttpForwarderError> {
        let target_stream = {
            let _ = DetourGuard::new();
            TcpStream::connect(connect_to).await?
        };

        let (http_request_sender, connection) = http1::handshake(target_stream).await?;

        // spawn a task to poll the connection.
        tokio::spawn(async move {
            if let Err(fail) = connection.await {
                error!("Error in http connection with addr {connect_to:?}: {fail:?}");
            }
        });

        Ok(HttpV1 {
            destination: connect_to,
            sender: http_request_sender,
        })
    }

    /// Starts the communication handling of `matched request -> user application -> response` by
    /// "listening" on the `request_receiver`.
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
