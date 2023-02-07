use std::net::SocketAddr;

use bytes::Bytes;
use futures::FutureExt;
use http_body_util::Full;
use hyper::client::conn::http2;
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

pub(super) struct V2 {
    address: SocketAddr,
    sender: http2::SendRequest<Full<Bytes>>,
}

impl V2 {
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
            let Self { address, sender } =
                ConnectionTask::<Self>::connect_to_application(self.address).await?;

            self.address = address;
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

impl ConnectionTask<V2> {
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

    async fn connect_to_application(connect_to: SocketAddr) -> Result<V2, HttpForwarderError> {
        let target_stream = {
            let _ = DetourGuard::new();
            TcpStream::connect(connect_to).await?
        };

        let (http_request_sender, connection) = http2::handshake(target_stream).await?;

        // spawn a task to poll the connection.
        tokio::spawn(async move {
            if let Err(fail) = connection.await {
                error!("Error in http connection with addr {connect_to:?}: {fail:?}");
            }
        });

        Ok(V2 {
            address: connect_to,
            sender: http_request_sender,
        })
    }

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
