use std::{future::Future, net::SocketAddr};

use bytes::Bytes;
use futures::FutureExt;
use http_body_util::Full;
use hyper::{client::conn::http2, rt::Executor};
use mirrord_protocol::{
    tcp::{HttpRequest, HttpResponse},
    ConnectionId, Port,
};
use tokio::{
    net::TcpStream,
    sync::mpsc::{Receiver, Sender},
};
use tracing::{debug, error};

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
        debug!("starting tokio executor for hyper HTTP/2");
        tokio::spawn(async move {
            let _ = DetourGuard::new();
            fut.await
        });
    }
}

impl ConnectionTask<V2> {
    #[tracing::instrument(level = "debug", skip(request_receiver, response_sender))]
    pub(super) async fn new(
        connect_to: SocketAddr,
        request_receiver: Receiver<HttpRequest>,
        response_sender: Sender<HttpResponse>,
        port: Port,
        connection_id: ConnectionId,
    ) -> Result<Self, HttpForwarderError> {
        println!("httpv2::new -> {connect_to:#?}");
        let http_version = Self::connect_to_application(connect_to).await?;

        Ok(Self {
            request_receiver,
            response_sender,
            port,
            connection_id,
            http_version,
        })
    }

    #[tracing::instrument(level = "debug")]
    async fn connect_to_application(connect_to: SocketAddr) -> Result<V2, HttpForwarderError> {
        println!("connect_to_application");
        let http_request_sender = {
            let _ = DetourGuard::new();
            let target_stream = TcpStream::connect(connect_to).await?;
            println!("{target_stream:#?}");

            let (http_request_sender, connection) = http2::Builder::new()
                .executor(TokioExecutor::default())
                .handshake(target_stream)
                .await?;
            // let (http_request_sender, connection) = http2::handshake(target_stream).await?;
            println!("{http_request_sender:#?}");
            println!("{connection:#?}");

            // spawn a task to poll the connection.
            tokio::spawn(async move {
                println!("connecting!");
                if let Err(fail) = connection.await {
                    error!("Error in http connection with addr {connect_to:?}: {fail:?}");
                }
            });

            http_request_sender
        };

        Ok(V2 {
            address: connect_to,
            sender: http_request_sender,
        })
    }

    #[tracing::instrument(level = "debug", skip(self))]
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
