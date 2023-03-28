//! Holds the implementation of [`Service`] for [`HyperHandler`] (HTTP/1).
//!
//! # [`HttpV1`]
//!
//! Handles HTTP/1 requests (with support for upgrades).
use core::{fmt::Debug, future::Future, pin::Pin};
use std::{net::SocketAddr, sync::Arc};

use bytes::Bytes;
use dashmap::DashMap;
use fancy_regex::Regex;
use http_body_util::Full;
use hyper::{
    body::Incoming,
    client::{self, conn::http1::SendRequest},
    header::UPGRADE,
    service::Service,
    Request, Response,
};
use mirrord_protocol::{ConnectionId, Port};
use tokio::{
    macros::support::poll_fn,
    net::TcpStream,
    sync::{mpsc::Sender, oneshot},
};
use tracing::error;

use super::{
    hyper_handler::{prepare_response, HyperHandler},
    HttpV, RawHyperConnection,
};
use crate::{
    steal::{http::error::HttpTrafficError, HandlerHttpRequest},
    util::ClientId,
};

/// Handles HTTP/1 requests (including upgrade requests, except HTTP/1 to HTTP/2).
///
/// See [`HyperHandler`] for more details.
///
/// Upgrade channel with the upgraded interceptor connection.
///
/// We use this channel to take control of the upgraded connection back from hyper.
#[derive(Debug)]
pub(crate) struct HttpV1(Option<oneshot::Sender<RawHyperConnection>>);

impl HttpV1 {
    fn take_upgrade_tx(&mut self) -> Option<oneshot::Sender<RawHyperConnection>> {
        self.0.take()
    }
}

impl HttpV for HttpV1 {
    type Sender = SendRequest<Incoming>;

    async fn connect(
        target_stream: TcpStream,
        upgrade_tx: Option<oneshot::Sender<RawHyperConnection>>,
    ) -> Result<Self::Sender, HttpTrafficError> {
        let (request_sender, mut connection) = client::conn::http1::handshake(target_stream)
            .await
            .inspect_err(|fail| error!("Handshake failed with {fail:#?}"))?;

        // We need this to progress the connection forward (hyper thing).
        tokio::spawn(async move {
            // The connection has to be kept alive for the manual handling of an HTTP upgrade.
            if let Err(fail) = poll_fn(|cx| connection.poll_without_shutdown(cx)).await {
                error!("Connection failed in unmatched with {fail:#?}");
            }

            // If this is not an upgrade, then we'll just drop the `Sender`, this is enough to
            // signal the `Receiver` in `filter.rs` that we're not dealing with an
            // upgrade request, and that the `HyperHandler` connection can be dropped.
            if let Some(sender) = upgrade_tx {
                let client::conn::http1::Parts { io, read_buf, .. } = connection.into_parts();

                let _ = sender
                    .send(RawHyperConnection {
                        stream: io,
                        unprocessed_bytes: read_buf,
                    })
                    .inspect_err(|_| error!("Failed sending interceptor connection!"));
            }
        });

        Ok(request_sender)
    }

    async fn send_request(
        sender: &mut Self::Sender,
        request: Request<Incoming>,
    ) -> Result<Response<Full<Bytes>>, HttpTrafficError> {
        prepare_response(
            sender
                .send_request(request)
                .await
                .inspect_err(|fail| error!("Failed hyper request sender with {fail:#?}"))?
                .into_parts(),
        )
        .await
    }

    fn is_upgrade(request: &Request<Incoming>) -> bool {
        request.headers().get(UPGRADE).is_some()
    }
}

impl HyperHandler<HttpV1> {
    /// Creates a new [`HyperHandler`] specifically tuned to handle HTTP/1 requests.
    pub(super) fn new(
        filters: Arc<DashMap<ClientId, Regex>>,
        matched_tx: Sender<HandlerHttpRequest>,
        connection_id: ConnectionId,
        port: Port,
        original_destination: SocketAddr,
        upgrade_tx: Option<oneshot::Sender<RawHyperConnection>>,
    ) -> Self {
        Self {
            filters,
            matched_tx,
            connection_id,
            port,
            original_destination,
            request_id: 0,
            handle_version: HttpV1(upgrade_tx),
        }
    }
}

impl Service<Request<Incoming>> for HyperHandler<HttpV1> {
    type Response = Response<Full<Bytes>>;

    type Error = HttpTrafficError;

    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    #[tracing::instrument(level = "trace", skip(self))]
    fn call(&mut self, request: Request<Incoming>) -> Self::Future {
        self.request_id += 1;

        Box::pin(HyperHandler::<HttpV1>::handle_request(
            request,
            self.original_destination,
            self.handle_version.take_upgrade_tx(),
            self.filters.clone(),
            self.port,
            self.connection_id,
            self.request_id,
            self.matched_tx.clone(),
        ))
    }
}
