//! Holds the implementation of [`Service`] for [`HyperHandler`] (HTTP/2).
//!
//! # [`HttpV2`]
//!
//! Handles HTTP/2 requests.
use core::{fmt::Debug, future::Future, pin::Pin};
use std::{net::SocketAddr, sync::Arc};

use bytes::Bytes;
use dashmap::DashMap;
use fancy_regex::Regex;
use futures::TryFutureExt;
use http_body_util::Full;
use hyper::{
    body::Incoming,
    client::{self, conn::http2::SendRequest},
    server::conn::http2,
    service::Service,
    Request, Response,
};
use mirrord_protocol::{ConnectionId, Port};
use tokio::{
    net::TcpStream,
    sync::{mpsc::Sender, oneshot},
};
use tracing::error;

use super::{
    filter::{close_connection, TokioExecutor},
    hyper_handler::{collect_response, prepare_response, HyperHandler},
    DefaultReversibleStream, HttpV, RawHyperConnection,
};
use crate::{
    steal::{http::error::HttpTrafficError, HandlerHttpRequest},
    util::ClientId,
};

/// Handles HTTP/2 requests.
///
/// See [`HyperHandler`] for more details.
#[derive(Debug)]
pub(crate) struct HttpV2;

impl HttpV for HttpV2 {
    type Sender = SendRequest<Incoming>;

    async fn serve_connection(
        stream: DefaultReversibleStream,
        original_destination: SocketAddr,
        connection_id: ConnectionId,
        filters: Arc<DashMap<ClientId, Regex>>,
        matched_tx: Sender<HandlerHttpRequest>,
        connection_close_sender: Sender<ConnectionId>,
    ) -> Result<(), HttpTrafficError> {
        http2::Builder::new(TokioExecutor::default())
            .serve_connection(
                stream,
                HyperHandler::<HttpV2>::new(
                    filters,
                    matched_tx,
                    connection_id,
                    original_destination.port(),
                    original_destination,
                ),
            )
            .await?;

        close_connection(connection_close_sender, connection_id).await
    }

    #[tracing::instrument(level = "trace")]
    async fn connect(
        target_stream: TcpStream,
        _: Option<oneshot::Sender<RawHyperConnection>>,
    ) -> Result<Self::Sender, HttpTrafficError> {
        let (request_sender, connection) =
            client::conn::http2::handshake(TokioExecutor::default(), target_stream)
                .await
                .inspect_err(|fail| error!("Handshake failed with {fail:#?}"))?;

        // We need this to progress the connection forward (hyper thing).
        tokio::spawn(async move {
            if let Err(fail) = connection.await {
                error!("Connection failed in unmatched with {fail:#?}");
            }
        });

        Ok(request_sender)
    }

    #[tracing::instrument(level = "trace")]
    async fn send_request(
        sender: &mut Self::Sender,
        request: Request<Incoming>,
    ) -> Result<Response<Full<Bytes>>, HttpTrafficError> {
        // Send the request to the original destination.
        prepare_response(
            sender
                .send_request(request)
                .inspect_err(|fail| error!("Failed hyper request sender with {fail:#?}"))
                .map_err(HttpTrafficError::from)
                .and_then(collect_response)
                .await?,
        )
        .await
    }

    #[tracing::instrument(level = "trace")]
    fn is_upgrade(_: &Request<Incoming>) -> bool {
        false
    }
}

impl HyperHandler<HttpV2> {
    /// Creates a new [`HyperHandler`] specifically tuned to handle HTTP/2 requests.
    #[tracing::instrument(level = "trace")]
    pub(crate) fn new(
        filters: Arc<DashMap<ClientId, Regex>>,
        matched_tx: Sender<HandlerHttpRequest>,
        connection_id: ConnectionId,
        port: Port,
        original_destination: SocketAddr,
    ) -> Self {
        Self {
            filters,
            matched_tx,
            connection_id,
            port,
            original_destination,
            request_id: 0,
            handle_version: HttpV2,
        }
    }
}

impl Service<Request<Incoming>> for HyperHandler<HttpV2> {
    type Response = Response<Full<Bytes>>;

    type Error = HttpTrafficError;

    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    #[tracing::instrument(level = "trace", skip(self))]
    fn call(&mut self, request: Request<Incoming>) -> Self::Future {
        self.request_id += 1;

        Box::pin(HyperHandler::<HttpV2>::handle_request(
            request,
            self.original_destination,
            None,
            self.filters.clone(),
            self.port,
            self.connection_id,
            self.request_id,
            self.matched_tx.clone(),
        ))
    }
}
