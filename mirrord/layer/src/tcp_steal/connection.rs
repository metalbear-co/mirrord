use std::{future::Future, net::SocketAddr};

use mirrord_protocol::{
    tcp::{HttpRequest, HttpResponse},
    ConnectionId, Port,
};
use tokio::{
    net::TcpStream,
    sync::mpsc::{Receiver, Sender},
};

use crate::{detour::DetourGuard, tcp_steal::HttpForwarderError};

pub(crate) struct ConnectionTask<HttpVersion> {
    pub(crate) request_receiver: Receiver<HttpRequest>,
    pub(crate) response_sender: Sender<HttpResponse>,
    pub(crate) port: Port,
    pub(crate) connection_id: ConnectionId,
    pub(crate) http_version: HttpVersion,
}

pub(super) trait HttpVersionT {
    type Sender;
    type Connection: Future + Send + 'static;

    /// Creates a new [`ConnectionTask`] that handles [`HttpV1`]/[`HttpV2`] requests.
    ///
    /// Connects to the user's application with `Self::connect_to_application`.
    fn new(connect_to: SocketAddr, http_request_sender: Self::Sender) -> Self;

    async fn handshake(
        target_stream: TcpStream,
    ) -> Result<(Self::Sender, Self::Connection), HttpForwarderError>;
}

impl<V> ConnectionT<V> for ConnectionTask<V> where V: HttpVersionT {}

pub(super) trait ConnectionT<V>: Sized
where
    V: HttpVersionT,
{
    async fn new(
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
            http_version,
        })
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

        Ok(HttpVersionT::new(connect_to, http_request_sender))
    }
}
