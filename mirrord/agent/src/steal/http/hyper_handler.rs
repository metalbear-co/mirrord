use core::{future::Future, pin::Pin, task::ready};
use std::{net::SocketAddr, sync::Arc};

use bytes::Bytes;
use dashmap::DashMap;
use fancy_regex::Regex;
use futures::{FutureExt, TryFutureExt};
use http_body_util::{combinators::BoxBody, BodyExt, Empty, Full};
use hyper::{
    body::Incoming,
    client,
    header::{SEC_WEBSOCKET_ACCEPT, UPGRADE},
    http::{self, request::Parts, Extensions, HeaderValue},
    service::Service,
    upgrade::{OnUpgrade, Upgraded},
    Request, Response, StatusCode, Version,
};
use mirrord_protocol::{ConnectionId, Port, RequestId};
use tokio::{
    io::{copy_bidirectional, AsyncReadExt, AsyncWriteExt},
    macros::support::poll_fn,
    net::TcpStream,
    sync::{
        mpsc::Sender,
        oneshot::{self, Receiver},
    },
};
use tracing::{debug, error, info};

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

    pub(super) upgrade_tx: Option<oneshot::Sender<Option<TcpStream>>>,
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

/// Handles the case when no filter matches a header in the request.
///
/// 1. Creates a [`hyper::client::conn::http1::Connection`] to the `original_destination`;
/// 2. Sends the [`Request`] to it, and awaits a [`Response`];
/// 3. Sends the [`HttpResponse`] back on the connected [`TcpStream`].
// #[tracing::instrument(level = "trace")]
async fn unmatched_request(
    request: Request<Incoming>,
    is_upgrade: bool,
    upgrade_tx: Option<oneshot::Sender<Option<TcpStream>>>,
    original_destination: SocketAddr,
) -> Result<Response<Full<Bytes>>, HttpTrafficError> {
    // TODO(alex): We need a "retry" mechanism here for the client handling part, when the server
    // closes a connection, the client could still be wanting to send a request, so we need to
    // re-connect and send.
    let tcp_stream = TcpStream::connect(original_destination)
        .await
        .inspect_err(|fail| error!("Failed connecting to original_destination with {fail:#?}"))?;

    // TODO(alex) [high] 2023-01-17: Can probably do the upgrade here, as we have the request and
    // the stream.
    let (mut request_sender, mut connection) = client::conn::http1::handshake(tcp_stream)
        .await
        .inspect_err(|fail| error!("Handshake failed with {fail:#?}"))?;

    // We need this to progress the connection forward (hyper thing).
    tokio::spawn(async move {
        if let Err(fail) = poll_fn(|cx| connection.poll_without_shutdown(cx)).await {
            error!("Connection failed in unmatched with {fail:#?}");
        }

        if is_upgrade {
            info!("time to handle the upgrade manually");
            let client::conn::http1::Parts { io, read_buf, .. } = connection.into_parts();

            if let Some(upgrade_tx) = upgrade_tx {
                upgrade_tx.send(Some(io)).unwrap();
            }
        } else {
            if let Some(upgrade_tx) = upgrade_tx {
                upgrade_tx.send(None).unwrap();
            }
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

    info!("Sending the unmatched response (could be the upgrade response from remote)");

    // Rebuild the `Response` after our fiddling.
    let response = Response::from_parts(parts, body.into());
    info!("the response for unmatched is {response:#?}");
    Ok(response)
}

impl Service<Request<Incoming>> for HyperHandler {
    type Response = Response<Full<Bytes>>;

    type Error = HttpTrafficError;

    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    // #[tracing::instrument(level = "debug", skip(self))]
    fn call(&mut self, request: Request<Incoming>) -> Self::Future {
        self.request_id += 1;

        let upgrade_tx = self.upgrade_tx.take();

        let original_destination = self.original_destination.clone();
        let filters = self.filters.clone();
        let port = self.port;
        let connection_id = self.connection_id;
        let request_id = self.request_id;
        let matched_tx = self.matched_tx.clone();

        let response = async move {
            if let Some(upgrade_to) = request.headers().get(UPGRADE).cloned() {
                info!("We have an upgrade request folks!");

                unmatched_request(request, true, upgrade_tx, original_destination).await
            } else if let Some(client_id) = request
                .headers()
                .iter()
                .map(|(header_name, header_value)| {
                    header_value
                        .to_str()
                        .map(|header_value| format!("{}: {}", header_name, header_value))
                })
                .find_map(|header| {
                    filters.iter().find_map(|filter| {
                        // TODO(alex) [low] 2022-12-23: Remove the `header` unwrap.
                        if filter.is_match(header.as_ref().unwrap()).unwrap() {
                            Some(*filter.key())
                        } else {
                            None
                        }
                    })
                })
            {
                let req = MatchedHttpRequest {
                    port,
                    connection_id,
                    client_id,
                    request_id,
                    request,
                };

                let (response_tx, response_rx) = oneshot::channel();
                let handler_request = HandlerHttpRequest {
                    request: req,
                    response_tx,
                };

                matched_request(handler_request, matched_tx, response_rx).await
            } else {
                unmatched_request(request, false, upgrade_tx, original_destination).await
            }
        };

        Box::pin(response)
    }
}
