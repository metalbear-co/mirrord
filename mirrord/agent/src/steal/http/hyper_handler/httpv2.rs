use core::{fmt::Debug, future::Future, pin::Pin};
use std::{net::SocketAddr, sync::Arc};

use bytes::Bytes;
use dashmap::DashMap;
use fancy_regex::Regex;
use http_body_util::Full;
use hyper::{body::Incoming, client, service::Service, Request, Response};
use mirrord_protocol::{ConnectionId, Port, RequestId};
use tokio::{net::TcpStream, sync::mpsc::Sender};
use tracing::error;

use super::{header_matches, matched_request, prepare_response, HyperHandler};
use crate::{
    steal::{http::error::HttpTrafficError, HandlerHttpRequest, MatchedHttpRequest},
    util::ClientId,
};

#[derive(Debug)]
pub(crate) struct HttpV2;

impl HyperHandler<HttpV2> {
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

        Box::pin(HttpV2::handle_request(
            request,
            self.original_destination,
            self.filters.clone(),
            self.port,
            self.connection_id,
            self.request_id,
            self.matched_tx.clone(),
        ))
    }
}

impl HttpV2 {
    #[tracing::instrument(level = "trace")]
    async fn unmatched_request(
        request: Request<Incoming>,
        original_destination: SocketAddr,
    ) -> Result<Response<Full<Bytes>>, HttpTrafficError> {
        // TODO(alex): We need a "retry" mechanism here for the client handling part, when the
        // server closes a connection, the client could still be wanting to send a request,
        // so we need to re-connect and send.
        let tcp_stream = TcpStream::connect(original_destination)
            .await
            .inspect_err(|fail| {
                error!("Failed connecting to original_destination with {fail:#?}")
            })?;

        let (mut request_sender, connection) = client::conn::http2::handshake(tcp_stream)
            .await
            .inspect_err(|fail| error!("Handshake failed with {fail:#?}"))?;

        // We need this to progress the connection forward (hyper thing).
        tokio::spawn(async move {
            // The connection has to be kept alive for the manual handling of an HTTP upgrade.
            if let Err(fail) = connection.await {
                error!("Connection failed in unmatched with {fail:#?}");
            }
        });

        prepare_response(
            // Send the request to the original destination.
            request_sender
                .send_request(request)
                .await
                .inspect_err(|fail| error!("Failed hyper request sender with {fail:#?}"))?
                .into_parts(),
        )
        .await
    }

    #[tracing::instrument(level = "trace")]
    async fn handle_request(
        request: Request<Incoming>,
        original_destination: SocketAddr,
        filters: Arc<DashMap<ClientId, Regex>>,
        port: Port,
        connection_id: ConnectionId,
        request_id: RequestId,
        matched_tx: Sender<HandlerHttpRequest>,
    ) -> Result<Response<Full<Bytes>>, HttpTrafficError> {
        if let Some(client_id) = header_matches(&request, &filters) {
            let request = MatchedHttpRequest {
                port,
                connection_id,
                client_id,
                request_id,
                request,
            };

            matched_request(request, matched_tx).await
        } else {
            Self::unmatched_request(request, original_destination).await
        }
    }
}
