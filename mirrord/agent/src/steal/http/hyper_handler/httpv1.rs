use core::{fmt::Debug, future::Future, pin::Pin};
use std::{net::SocketAddr, sync::Arc};

use bytes::Bytes;
use dashmap::DashMap;
use fancy_regex::Regex;
use http_body_util::Full;
use hyper::{body::Incoming, client, header::UPGRADE, service::Service, Request, Response};
use mirrord_protocol::{ConnectionId, Port, RequestId};
use tokio::{
    macros::support::poll_fn,
    net::TcpStream,
    sync::{mpsc::Sender, oneshot},
};
use tracing::error;

use super::{header_matches, matched_request, prepare_response, HyperHandler, RawHyperConnection};
use crate::{
    steal::{http::error::HttpTrafficError, HandlerHttpRequest, MatchedHttpRequest},
    util::ClientId,
};

/// Handles HTTP/1 requests (including upgrade requests, except HTTP/1 to HTTP/2).
///
/// See [`HyperHandler`] for more details.
#[derive(Debug)]
pub(crate) struct HttpV1 {
    /// Upgrade channel with the upgraded interceptor connection.
    ///
    /// We use this channel to take control of the upgraded connection back from hyper.
    upgrade_tx: Option<oneshot::Sender<RawHyperConnection>>,
}

impl HyperHandler<HttpV1> {
    /// Creates a new [`HyperHandler`] specifically tuned to handle HTTP/1 requests.
    pub(crate) fn new(
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
            handle_version: HttpV1 { upgrade_tx },
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

        Box::pin(HttpV1::handle_request(
            request,
            self.original_destination,
            self.handle_version.upgrade_tx.take(),
            self.filters.clone(),
            self.port,
            self.connection_id,
            self.request_id,
            self.matched_tx.clone(),
        ))
    }
}

impl HttpV1 {
    /// Handles the case when no filter matches a header in the request (or we have an HTTP upgrade
    /// request).
    ///
    /// # Flow
    ///
    /// 1. Creates a [`http1::Connection`](hyper::client::conn::http1::Connection) to the
    /// `original_destination`;
    ///
    /// 2. Sends the [`Request`] to it, and awaits a [`Response`];
    ///
    /// 3. Sends the [`HttpResponse`] back on the connected [`TcpStream`];
    ///
    /// ## Special case (HTTP upgrade request)
    ///
    /// If the [`Request`] is an HTTP upgrade request, then we send the `original_destination`
    /// connection, through `upgrade_tx`, to be handled in [`filter_task`](super::filter_task).
    ///
    /// - Why not use [`hyper::upgrade::on`]?
    ///
    /// [`hyper::upgrade::on`] requires the original [`Request`] as a parameter, due to it having
    /// the [`OnUpgrade`](hyper::upgrade::OnUpgrade) receiver tucked inside as an
    /// [`Extensions`](http::extensions::Extensions)
    /// (you can see this [here](https://docs.rs/hyper/1.0.0-rc.2/src/hyper/upgrade.rs.html#73)).
    ///
    /// [`hyper::upgrade::on`] polls this receiver to identify if this an HTTP upgrade request.
    ///
    /// The issue for us is that we need to send the [`Request`] to its original destination, with
    /// [`SendRequest`](hyper::client::conn::http1::SendRequest), which takes ownership of the
    /// request, prohibiting us to also pass it to the proper hyper upgrade handler.
    ///
    /// Trying to copy the [`Request`] is futile, as we can't copy the `OnUpgrade` extension, and if
    /// we move it from the original `Request` to a copied `Request`, the channel will never
    /// receive anything due it being in a different `Request` than the one we actually send to
    /// the hyper machine.
    #[tracing::instrument(level = "trace")]
    async fn unmatched_request(
        request: Request<Incoming>,
        upgrade_tx: Option<oneshot::Sender<RawHyperConnection>>,
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

        let (mut request_sender, mut connection) = client::conn::http1::handshake(tcp_stream)
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

    /// Handles the incoming HTTP/1 [`Request`].
    ///
    /// Checks if this [`Request`] contains an upgrade header, and if not, then checks if a header
    /// matches one of the user specified filters.
    ///
    /// Helper function due to the fact that [`Service::call`] is not an `async` function.
    #[allow(clippy::too_many_arguments)]
    #[tracing::instrument(level = "trace")]
    async fn handle_request(
        request: Request<Incoming>,
        original_destination: SocketAddr,
        upgrade_tx: Option<oneshot::Sender<RawHyperConnection>>,
        filters: Arc<DashMap<ClientId, Regex>>,
        port: Port,
        connection_id: ConnectionId,
        request_id: RequestId,
        matched_tx: Sender<HandlerHttpRequest>,
    ) -> Result<Response<Full<Bytes>>, HttpTrafficError> {
        if request.headers().get(UPGRADE).is_some() {
            Self::unmatched_request(request, upgrade_tx, original_destination).await
        } else if let Some(client_id) = header_matches(&request, &filters) {
            let request = MatchedHttpRequest {
                port,
                connection_id,
                client_id,
                request_id,
                request,
            };

            matched_request(request, matched_tx).await
        } else {
            Self::unmatched_request(request, None, original_destination).await
        }
    }
}
