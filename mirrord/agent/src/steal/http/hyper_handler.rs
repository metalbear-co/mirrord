use core::{future::Future, pin::Pin};
use std::{net::SocketAddr, sync::Arc};

use bytes::Bytes;
use dashmap::DashMap;
use fancy_regex::Regex;
use futures::TryFutureExt;
use http_body_util::{BodyExt, Full};
use hyper::{body::Incoming, client, header::UPGRADE, http, service::Service, Request, Response};
use mirrord_protocol::{ConnectionId, Port, RequestId};
use tokio::{
    macros::support::poll_fn,
    net::TcpStream,
    sync::{
        mpsc::Sender,
        oneshot::{self, Receiver},
    },
};
use tracing::error;

use super::error::HttpTrafficError;
use crate::{
    steal::{HandlerHttpRequest, MatchedHttpRequest},
    util::ClientId,
};

/// Used to pass data to the [`Service`] implementation of [`hyper`].
///
/// Think of this struct as if it was a bunch of function arguments that are passed down to
/// [`Service::call`] method.
///
/// Each [`TcpStream`] connection (for stealer) requires this.
#[derive(Debug)]
pub(super) struct HyperHandler {
    /// The (shared with the stealer) HTTP filter regexes that are used to filter traffic for this
    /// particular connection.
    pub(super) filters: Arc<DashMap<ClientId, Regex>>,

    /// [`Sender`] part of the channel used to communicate with the agent that we have a
    /// [`MatchedHttpRequest`], and it should be forwarded to the layer.
    pub(super) matched_tx: Sender<HandlerHttpRequest>,

    /// Identifies this [`TcpStream`] connection.
    pub(crate) connection_id: ConnectionId,

    /// The port we're filtering HTTP traffic on.
    pub(crate) port: Port,

    /// The original [`SocketAddr`] of the connection we're intercepting.
    ///
    /// Used for the case where we have an unmatched request (HTTP request did not match any of the
    /// `filters`).
    pub(crate) original_destination: SocketAddr,

    /// Keeps track of which HTTP request we're dealing with, so we don't mix up [`Request`]s.
    pub(crate) request_id: RequestId,

    pub(super) upgrade_tx: Option<oneshot::Sender<RawHyperConnection>>,
}

/// Holds a connection and bytes that were left unprocessed by the [`hyper`] state machine.
#[derive(Debug)]
pub(super) struct RawHyperConnection {
    /// The connection as a raw IO object.
    pub(super) stream: TcpStream,

    /// Bytes that were not handled by [`hyper`].
    pub(super) unprocessed_bytes: Bytes,
}

/// Sends a [`MatchedHttpRequest`] through `tx` to be handled by the stealer -> layer.
#[tracing::instrument(level = "trace", skip(matched_tx, response_rx))]
async fn matched_request(
    request: HandlerHttpRequest,
    matched_tx: Sender<HandlerHttpRequest>,
    response_rx: Receiver<Response<Full<Bytes>>>,
) -> Result<Response<Full<Bytes>>, HttpTrafficError> {
    matched_tx
        .send(request)
        .map_err(HttpTrafficError::from)
        .await?;

    let (mut parts, body) = response_rx.await?.into_parts();
    parts.headers.remove(http::header::CONTENT_LENGTH);
    parts.headers.remove(http::header::TRANSFER_ENCODING);

    Ok(Response::from_parts(parts, body))
}

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
/// [`hyper::upgrade::on`] requires the original [`Request`] as a parameter, due to it having the
/// [`OnUpgrade`](hyper::upgrade::OnUpgrade) receiver tucked inside as an
/// [`Extensions`](http::extensions::Extensions)
/// (you can see this [here](https://docs.rs/hyper/1.0.0-rc.2/src/hyper/upgrade.rs.html#73)).
///
/// [`hyper::upgrade::on`] polls this receiver to identify if this an HTTP upgrade request.
///
/// The issue for us is that we need to send the [`Request`] to its original destination, with
/// [`SendRequest`](hyper::client::conn::http1::SendRequest), which takes ownership of the request,
/// prohibiting us to also pass it to the proper hyper upgrade handler.
///
/// Trying to copy the [`Request`] is futile, as we can't copy the `OnUpgrade` extension, and if we
/// move it from the original `Request` to a copied `Request`, the channel will never receive
/// anything due it being in a different `Request` than the one we actually send to the hyper
/// machine.
#[tracing::instrument(level = "trace")]
async fn unmatched_request<const IS_UPGRADE: bool>(
    request: Request<Incoming>,
    upgrade_tx: Option<oneshot::Sender<RawHyperConnection>>,
    original_destination: SocketAddr,
) -> Result<Response<Full<Bytes>>, HttpTrafficError> {
    // TODO(alex): We need a "retry" mechanism here for the client handling part, when the server
    // closes a connection, the client could still be wanting to send a request, so we need to
    // re-connect and send.
    let tcp_stream = TcpStream::connect(original_destination)
        .await
        .inspect_err(|fail| error!("Failed connecting to original_destination with {fail:#?}"))?;

    let (mut request_sender, mut connection) = client::conn::http1::handshake(tcp_stream)
        .await
        .inspect_err(|fail| error!("Handshake failed with {fail:#?}"))?;

    // We need this to progress the connection forward (hyper thing).
    tokio::spawn(async move {
        // The connection has to be kept alive for the manual handling of an HTTP upgrade.
        if let Err(fail) = poll_fn(|cx| connection.poll_without_shutdown(cx)).await {
            error!("Connection failed in unmatched with {fail:#?}");
        }

        // If this is not an upgrade, then we'll just drop the `Sender`, this is enough to signal
        // the `Receiver` in `filter.rs` that we're not dealing with an upgrade request, and that
        // the `HyperHandler` connection can be dropped.
        if IS_UPGRADE {
            let client::conn::http1::Parts { io, read_buf, .. } = connection.into_parts();

            let _ = upgrade_tx
                .map(|sender| {
                    sender.send(RawHyperConnection {
                        stream: io,
                        unprocessed_bytes: read_buf,
                    })
                })
                .transpose()
                .inspect_err(|_| error!("Failed sending interceptor connection!"));
        }
    });

    // Send the request to the original destination.
    let (mut parts, body) = request_sender
        .send_request(request)
        .await
        .inspect_err(|fail| error!("Failed hyper request sender with {fail:#?}"))?
        .into_parts();

    // Remove headers that would be invalid due to us fiddling with the `body`.
    let body = body.collect().await?.to_bytes();
    parts.headers.remove(http::header::CONTENT_LENGTH);
    parts.headers.remove(http::header::TRANSFER_ENCODING);

    // Rebuild the `Response` after our fiddling.
    Ok(Response::from_parts(parts, body.into()))
}

impl Service<Request<Incoming>> for HyperHandler {
    type Response = Response<Full<Bytes>>;

    type Error = HttpTrafficError;

    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    #[tracing::instrument(level = "trace", skip(self))]
    fn call(&mut self, request: Request<Incoming>) -> Self::Future {
        self.request_id += 1;

        let response = |original_destination,
                        upgrade_tx,
                        filters: Arc<DashMap<ClientId, Regex>>,
                        port,
                        connection_id,
                        request_id,
                        matched_tx| async move {
            if request.headers().get(UPGRADE).is_some() {
                unmatched_request::<true>(request, upgrade_tx, original_destination).await
            } else if let Some(client_id) = request
                .headers()
                .iter()
                .map(|(header_name, header_value)| {
                    header_value
                        .to_str()
                        .map(|header_value| format!("{header_name}: {header_value}"))
                })
                .find_map(|header| {
                    filters.iter().find_map(|filter| {
                        filter
                            .is_match(
                                header
                                    .as_ref()
                                    .expect("The header value has to be convertible to `String`!"),
                            )
                            .expect("Something went wrong in the regex matcher!")
                            .then_some(*filter.key())
                    })
                })
            {
                let request = MatchedHttpRequest {
                    port,
                    connection_id,
                    client_id,
                    request_id,
                    request,
                };

                let (response_tx, response_rx) = oneshot::channel();
                let handler_request = HandlerHttpRequest {
                    request,
                    response_tx,
                };

                matched_request(handler_request, matched_tx, response_rx).await
            } else {
                unmatched_request::<false>(request, upgrade_tx, original_destination).await
            }
        };

        Box::pin(response(
            self.original_destination,
            self.upgrade_tx.take(),
            self.filters.clone(),
            self.port,
            self.connection_id,
            self.request_id,
            self.matched_tx.clone(),
        ))
    }
}
