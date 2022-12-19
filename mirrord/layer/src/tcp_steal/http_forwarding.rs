use std::collections::HashMap;

use bytes::Bytes;
use http_body_util::Full;
use hyper::client::conn::http1::{handshake, SendRequest};
use mirrord_protocol::{
    tcp::{HttpRequest, HttpResponse},
    ConnectionId, Port,
};
use thiserror::Error;
use tokio::{
    net::TcpStream,
    sync::mpsc::{channel, error::SendError, Receiver, Sender},
};
use tracing::{error, trace};

use crate::{detour::DetourGuard, error::LayerError, tcp::TcpHandler, tcp_steal::TcpStealHandler};

#[derive(Error, Debug)]
pub(crate) enum HttpForwarderError {
    #[error("HTTP Forwarder: Failed to send connection id for closing with error: {0}.")]
    ConnectionCloseSend(#[from] SendError<u64>),

    #[error("HTTP Forwarder: Failed to send http response to main layer task with error: {0}.")]
    ResponseSend(#[from] SendError<HttpResponse>),

    #[error("HTTP Forwarder: Failed to send http request HTTP client task with error: {0}.")]
    Request2ClientSend(#[from] SendError<HttpRequest>),

    #[error("HTTP Forwarder: Could not send http request to application or receive its response with error: {0}.")]
    HttpForwarding(#[from] hyper::Error),

    #[error("HTTP Forwarder: TCP connection failed with error: {0}.")]
    TcpStream(#[from] std::io::Error),
}

/// Manages the forwarding of all the HTTP requests in all HTTP connections of this layer.
pub(super) struct HttpForwarder {
    /// Mapping of a ConnectionId to a sender that sends HTTP requests over to a task that is
    /// running an http client for this connection.
    request_senders: HashMap<ConnectionId, Sender<HttpRequest>>,

    /// Sender of responses from within an http client task back to the main layer task.
    /// This sender is cloned and moved into those tasks.
    response_sender: Sender<HttpResponse>,

    /// Receives responses in the main layer task sent from all http client tasks.
    response_receiver: Receiver<HttpResponse>,
}

impl Default for HttpForwarder {
    fn default() -> Self {
        // TODO: buffer size?
        let (response_sender, response_receiver) = channel(1024);
        Self {
            request_senders: HashMap::with_capacity(8),
            response_sender,
            response_receiver,
        }
    }
}

impl HttpForwarder {
    /// Wait for the next HTTP response by the application to a stolen request.
    /// Returns responses from all ports and connections.
    pub async fn next_response(&mut self) -> Option<HttpResponse> {
        self.response_receiver.recv().await
    }

    /// Send a filtered HTTP request to the application in the appropriate port.
    /// If this is the first filtered HTTP from its connection to arrive at this layer, a new
    /// connection will be started for it, otherwise it will be sent in the existing connection.
    pub(crate) async fn forward_request(
        &mut self,
        request: HttpRequest,
    ) -> Result<(), HttpForwarderError> {
        if let Some(sender) = self.request_senders.get_mut(&request.connection_id) {
            sender.send(request).await?
        } else {
            self.create_http_connection(request).await?
        }
        Ok(())
    }

    /// Manage a single tcp connection, forward requests, wait for responses, send responses back.
    async fn connection_task(
        mut request_receiver: Receiver<HttpRequest>,
        mut sender: SendRequest<Full<Bytes>>,
        response_sender: Sender<HttpResponse>,
        port: Port,
        connection_id: ConnectionId,
    ) -> Result<(), HttpForwarderError> {
        // Listen for more requests in this connection and forward them to app.
        while let Some(req) = request_receiver.recv().await {
            let request_id = req.request_id;
            // Send to application.
            let res = sender.send_request(req.request.into()).await?;
            let res =
                HttpResponse::from_hyper_response(res, port, connection_id, request_id).await?;
            // Send response back to forwarder.
            response_sender.send(res).await?;
        }
        Ok(())
    }

    /// Create a new TCP connection with the application to send all the filtered HTTP requests
    /// from this connection in.
    /// Spawn a task that receives requests on a channel and sends them to the application on that
    /// new TCP connection. The sender of that channel is stored in [`self.request_senders`].
    /// The requests from the tests will arrive together with responses from all other connections
    /// at [`self.response_receiver`].
    async fn create_http_connection(
        &mut self,
        http_request: HttpRequest,
        tcp_steal_handler: &TcpStealHandler,
    ) -> Result<(), HttpForwarderError> {
        let listen = tcp_steal_handler
            .ports()
            .get(&destination_port)
            .ok_or(LayerError::PortNotFound(destination_port))?;
        let target_steram = {
            let _ = DetourGuard::new();
            TcpStream::connect(format!("localhost:{}", http_request.port)).await?;
        };
        let (sender, connection): (SendRequest<Full<Bytes>>, _) = handshake(target_stream).await?;
        let connection_id = http_request.connection_id;
        let port = http_request.port;

        // spawn a task to poll the connection and drive the HTTP state
        tokio::spawn(async move {
            if let Err(e) = connection.await {
                error!(
                    "Error in http connection {} on port {}: {}",
                    connection_id, port, e
                );
            }
        });

        let (request_sender, request_receiver) = channel(1024);

        let response_sender = self.response_sender.clone();

        tokio::spawn(async move {
            if let Err(e) = Self::connection_task(
                request_receiver,
                sender,
                response_sender,
                port,
                connection_id,
            )
            .await
            {
                error!(
                    "Error while forwarding http connection {connection_id} (port {port}): {e:?}."
                )
            } else {
                trace!(
                    "Filtered http connection {connection_id} (port {port}) closed without errors."
                )
            }
        });

        request_sender.send(http_request).await?;
        // Give the forwarder a channel to send the task new requests from the same connection.
        self.request_senders.insert(connection_id, request_sender);

        Ok(())
    }
}
