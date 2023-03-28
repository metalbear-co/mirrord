//! # [`HttpV`]
//!
//! # [`ConnectionTask`]
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

/// Handles the connection between our _middle-man_ stream and the target `destination`.
///
/// # Details
///
/// Mainly responsible for starting a task that will get requests through `request_receiver` and
/// send them through `response_sender`, which is where we get the responses that will be sent back
/// to the agent, and finally back to the remote pod.
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

/// Handles the differences between hyper's HTTP/1 and HTTP/2 connections.
///
/// Thanks to this trait being implemented for both [`v1::HttpV1`] and [`v2::HttpV2`], we can have
/// most of the implementation for dealing with requests in [`ConnectionTask`].
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

    /// Takes the [`HttpV::Sender`], inserting `None` back into this instance.
    ///
    /// # Usage
    ///
    /// Mainly used for our retry connection mechanism, where we create a new [`HttpV`] implementor,
    /// but we want to swap the "failed attempt" [`HttpV::Sender`] with the newly created one.
    fn take_sender(self) -> Self::Sender;

    /// Sets this instance's [`HttpV::Sender`].
    ///
    /// # Usage
    ///
    /// Required as part of the [`HttpV::take_sender`] retry mechanism.
    fn set_sender(&mut self, new_sender: Self::Sender);

    /// Sends the [`HttpRequest`] to its destination.
    ///
    /// Required due to this mechanism having distinct types;
    /// HTTP/1 with [`http1::SendRequest`], and HTTP/2 with [`http2::SendRequest`].
    ///
    /// [`http1::SendRequest`]: hyper::client::conn::http1::SendRequest
    /// [`http2::SendRequest`]: hyper::client::conn::http2::SendRequest
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
    /// Creates a new [`ConnectionTask`] for handling HTTP/V requests.
    ///
    /// Has a side-effect of connecting to the application with the (aptly named)
    /// `connect_to_application`.
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
