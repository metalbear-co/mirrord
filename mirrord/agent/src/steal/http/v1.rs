//! Holds the implementation of [`Service`] for [`HyperHandler`] (HTTP/1).
//!
//! # [`HttpV1`]
//!
//! Handles HTTP/1 requests (with support for upgrades).
use core::{fmt::Debug, future::Future, pin::Pin};
use std::{
    net::SocketAddr,
    sync::{atomic::Ordering, Arc, Mutex},
};

use dashmap::DashMap;
use futures::TryFutureExt;
use http_body_util::{combinators::BoxBody, BodyExt};
use hyper::{
    body::Incoming,
    client::{self, conn::http1::SendRequest},
    header::UPGRADE,
    server::{self, conn::http1},
    service::Service,
    Request,
};
use hyper_util::rt::TokioIo;
use mirrord_protocol::{ConnectionId, Port};
use tokio::{
    io::{copy_bidirectional, AsyncWriteExt},
    macros::support::poll_fn,
    net::TcpStream,
    sync::{mpsc::Sender, oneshot},
};
use tracing::error;

use super::{
    filter::close_connection, hyper_handler::HyperHandler, DefaultReversibleStream, HttpFilter,
    HttpV, RawHyperConnection, Response,
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
pub(crate) struct HttpV1(Mutex<Option<oneshot::Sender<RawHyperConnection>>>);

impl HttpV1 {
    fn new(upgrate_tx: Option<oneshot::Sender<RawHyperConnection>>) -> Self {
        Self(Mutex::new(upgrate_tx))
    }

    fn take_upgrade_tx(&self) -> Option<oneshot::Sender<RawHyperConnection>> {
        self.0.lock().expect("poisoned lock").take()
    }
}

impl HttpV for HttpV1 {
    type Sender = SendRequest<Incoming>;

    async fn serve_connection(
        stream: DefaultReversibleStream,
        original_destination: SocketAddr,
        connection_id: ConnectionId,
        filters: Arc<DashMap<ClientId, HttpFilter>>,
        matched_tx: Sender<HandlerHttpRequest>,
        connection_close_sender: Sender<ConnectionId>,
    ) -> Result<(), HttpTrafficError> {
        // Contains the upgraded interceptor connection, if any.
        let (upgrade_tx, upgrade_rx) = oneshot::channel::<RawHyperConnection>();

        // We have to keep the connection alive to handle a possible upgrade request
        // manually.
        let server::conn::http1::Parts {
            io: client_agent, // i.e. browser-agent connection
            read_buf: agent_unprocessed,
            ..
        } = http1::Builder::new()
            .preserve_header_case(true)
            .serve_connection(
                TokioIo::new(stream),
                HyperHandler::<HttpV1>::new(
                    filters,
                    matched_tx,
                    connection_id,
                    original_destination.port(),
                    original_destination,
                    Some(upgrade_tx),
                ),
            )
            .without_shutdown()
            .await?;

        if let Ok(RawHyperConnection {
            stream: mut agent_remote, // i.e. agent-original destination connection
            unprocessed_bytes: client_unprocessed,
        }) = upgrade_rx.await
        {
            // Send the data we received from the client, and have not processed as
            // HTTP, to the original destination.
            agent_remote.write_all(&agent_unprocessed).await?;

            let mut client_agent = client_agent.into_inner();
            // Send the data we received from the original destination, and have not
            // processed as HTTP, to the client.
            client_agent.write_all(&client_unprocessed).await?;

            // Now both the client and original destinations should be in sync, so we
            // can just copy the bytes from one into the other.
            copy_bidirectional(&mut client_agent, &mut agent_remote).await?;
        }

        close_connection(connection_close_sender, connection_id).await
    }

    #[tracing::instrument(level = "trace")]
    async fn connect(
        target_stream: TcpStream,
        upgrade_tx: Option<oneshot::Sender<RawHyperConnection>>,
    ) -> Result<Self::Sender, HttpTrafficError> {
        let (request_sender, mut connection) =
            client::conn::http1::handshake(TokioIo::new(target_stream))
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
                        stream: io.into_inner(),
                        unprocessed_bytes: read_buf,
                    })
                    .inspect_err(|_| error!("Failed sending interceptor connection!"));
            }
        });

        Ok(request_sender)
    }

    #[tracing::instrument(level = "trace")]
    async fn send_request(
        sender: &mut Self::Sender,
        request: Request<Incoming>,
    ) -> Result<Response, HttpTrafficError> {
        sender
            .send_request(request)
            .inspect_err(|fail| error!("Failed hyper request sender with {fail:#?}"))
            .map_err(HttpTrafficError::from)
            .await
            .map(|response| response.map(|body| BoxBody::new(body.map_err(HttpTrafficError::from))))
    }

    #[tracing::instrument(level = "trace")]
    fn is_upgrade(request: &Request<Incoming>) -> bool {
        request.headers().get(UPGRADE).is_some()
    }
}

impl HyperHandler<HttpV1> {
    /// Creates a new [`HyperHandler`] specifically tuned to handle HTTP/1 requests.
    #[tracing::instrument(level = "trace")]
    pub(super) fn new(
        filters: Arc<DashMap<ClientId, HttpFilter>>,
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
            next_request_id: Default::default(),
            handle_version: HttpV1::new(upgrade_tx),
        }
    }
}

impl Service<Request<Incoming>> for HyperHandler<HttpV1> {
    type Response = Response;

    type Error = HttpTrafficError;

    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    #[tracing::instrument(level = "trace", skip(self))]
    fn call(&self, request: Request<Incoming>) -> Self::Future {
        let request_id = self.next_request_id.fetch_add(1, Ordering::Relaxed);

        Box::pin(HyperHandler::<HttpV1>::handle_request(
            request,
            self.original_destination,
            self.handle_version.take_upgrade_tx(),
            self.filters.clone(),
            self.port,
            self.connection_id,
            request_id,
            self.matched_tx.clone(),
        ))
    }
}
