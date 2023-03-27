use std::{future::Future, net::SocketAddr};

use futures::FutureExt;
use hyper::{body::Incoming, Response};
use mirrord_protocol::{
    tcp::{HttpRequest, HttpResponse},
    ConnectionId, Port,
};
use tokio::{
    net::TcpStream,
    sync::mpsc::{Receiver, Sender},
};

use super::handle_response;
use crate::{detour::DetourGuard, tcp_steal::HttpForwarderError};

pub(super) mod v1;
pub(super) mod v2;

pub(super) struct ConnectionTask<HttpVersion: HttpV> {
    request_receiver: Receiver<HttpRequest>,
    response_sender: Sender<HttpResponse>,
    port: Port,
    connection_id: ConnectionId,

    /// Address we're connecting to.
    destination: SocketAddr,

    /// Handles the differences between [`http1`] and [`http2`] in [`hyper`].
    http_version: HttpVersion,
}

pub(super) trait HttpV: Sized {
    type Sender;
    type Connection: Future + Send + 'static;

    /// Creates a new [`ConnectionTask`] that handles [`HttpV1`]/[`HttpV2`] requests.
    ///
    /// Connects to the user's application with `Self::connect_to_application`.
    fn new(http_request_sender: Self::Sender) -> Self;

    /// Calls the appropriate `HTTP/V` `handshake` method.
    async fn handshake(
        target_stream: TcpStream,
    ) -> Result<(Self::Sender, Self::Connection), HttpForwarderError>;

    fn take_sender(self) -> Self::Sender;
    fn set_sender(&mut self, new_sender: Self::Sender);

    async fn send_request(&mut self, request: HttpRequest) -> hyper::Result<Response<Incoming>>;

    /// Sends the [`HttpRequest`] through `Self::sender`, converting the response into a
    /// [`HttpResponse`].
    async fn send_http_request_to_application(
        &mut self,
        request: HttpRequest,
        port: Port,
        connection_id: ConnectionId,
        destination: SocketAddr,
    ) -> Result<HttpResponse, HttpForwarderError> {
        let request_id = request.request_id;

        let response = self
            .send_request(request.clone())
            .map(|response| handle_response(request, response, port, connection_id, request_id))
            .await
            .await;

        // Retry once if the connection was closed.
        if let Err(HttpForwarderError::ConnectionClosedTooSoon(request)) = response {
            // Create a new `HttpVersion` handler for this second attempt.
            let http_version = ConnectionTask::<Self>::connect_to_application(destination).await?;

            // Swap the old `HttpVersion` handler with the one we just created.
            self.set_sender(http_version.take_sender());

            Ok(self
                .send_request(request.clone())
                .map(|response| handle_response(request, response, port, connection_id, request_id))
                .await
                .await?)
        } else {
            response
        }
    }
}

impl<V> ConnectionTask<V>
where
    V: HttpV,
{
    pub(super) async fn new(
        connect_to: SocketAddr,
        request_receiver: Receiver<HttpRequest>,
        response_sender: Sender<HttpResponse>,
        port: Port,
        connection_id: ConnectionId,
    ) -> Result<ConnectionTask<V>, HttpForwarderError> {
        let http_version = Self::connect_to_application(connect_to).await?;

        Ok(ConnectionTask {
            request_receiver,
            response_sender,
            port,
            connection_id,
            destination: connect_to,
            http_version,
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
            destination,
            mut http_version,
        } = self;

        while let Some(request) = request_receiver.recv().await {
            let response = http_version
                .send_http_request_to_application(request, port, connection_id, destination)
                .await?;

            response_sender.send(response).await?;
        }

        Ok(())
    }

    /// Creates a client HTTP [`http1::Connection`]/[`http2::Connection`] to the user's application.
    ///
    /// Requests that match the user specified filter will be sent through this connection to the
    /// user.
    async fn connect_to_application(connect_to: SocketAddr) -> Result<V, HttpForwarderError> {
        let target_stream = {
            let _ = DetourGuard::new();
            TcpStream::connect(connect_to).await?
        };

        let (http_request_sender, connection) = V::handshake(target_stream).await?;

        // spawn a task to poll the connection.
        tokio::spawn(async move {
            connection.await;
        });

        Ok(HttpV::new(http_request_sender))
    }
}
