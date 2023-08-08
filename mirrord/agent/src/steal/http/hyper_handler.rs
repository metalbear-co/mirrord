//! The core part of the whole HTTP traffic stealing feature.
//!
//! # [`HyperHandler`]
//!
//! # [`RawHyperConnection`]
use core::fmt::Debug;
use std::{
    net::SocketAddr,
    sync::{atomic::AtomicU16, Arc},
};

use bytes::Bytes;
use dashmap::DashMap;
use futures::TryFutureExt;
use hyper::{body::Incoming, Request};
use mirrord_protocol::{ConnectionId, Port, RequestId};
use tokio::{
    net::TcpStream,
    sync::{mpsc::Sender, oneshot},
};
use tracing::error;

use super::{
    error::HttpTrafficError,
    filter::{HttpFilter, RequestMatch},
    HttpV, Response,
};
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
pub(super) struct HyperHandler<V>
where
    V: HttpV + Debug,
{
    /// The (shared with the stealer) HTTP filter regexes that are used to filter traffic for this
    /// particular connection.
    pub(super) filters: Arc<DashMap<ClientId, HttpFilter>>,

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
    pub(crate) next_request_id: AtomicU16,

    pub(super) handle_version: V,
}

/// Holds a connection and bytes that were left unprocessed by the [`hyper`] state machine.
#[derive(Debug)]
pub(super) struct RawHyperConnection {
    /// The connection as a raw IO object.
    pub(super) stream: TcpStream,

    /// Bytes that were not handled by [`hyper`].
    pub(super) unprocessed_bytes: Bytes,
}

/// Checks if the [`Request`]'s  matches any of the `filters`.
fn match_request(
    request: &Request<Incoming>,
    filters: &Arc<DashMap<ClientId, HttpFilter>>,
) -> Option<ClientId> {
    let mut request = request.into();
    filters.iter().find_map(|entry| {
        if entry.value().matches(&mut request) {
            Some(*entry.key())
        } else {
            None
        }
    })
}

/// Sends a [`MatchedHttpRequest`] through `tx` to be handled by the stealer -> layer,
/// and then waits for the response and returns it once it's there.
#[tracing::instrument(level = "trace", skip(matched_tx))]
async fn matched_request(
    request: MatchedHttpRequest,
    matched_tx: Sender<HandlerHttpRequest>,
) -> Result<Response, HttpTrafficError> {
    let (response_tx, response_rx) = oneshot::channel();
    let request = HandlerHttpRequest {
        request,
        response_tx,
    };

    matched_tx
        .send(request)
        .map_err(HttpTrafficError::from)
        .await?;

    response_rx.await.map_err(HttpTrafficError::from)
}

impl<V> HyperHandler<V>
where
    V: HttpV + Debug,
{
    /// Handles the case when no filter matches a header in the request (or we have an HTTP upgrade
    /// request).
    ///
    /// # HTTP/1
    ///
    /// ## Flow
    ///
    /// 1. Creates a [`http1::Connection`](hyper::client::conn::http1::Connection) to the
    /// `original_destination`;
    ///
    /// 2. Sends the [`Request`] to it, and awaits a [`Response`];
    ///
    /// 3. Sends the [`HttpResponse`] back on the connected [`TcpStream`];
    ///
    /// ### Special case (HTTP upgrade request)
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
    ///
    /// # HTTP/2
    ///
    /// ## Flow
    ///
    /// 1. Creates a [`http2::Connection`](hyper::client::conn::http2::Connection) to the
    /// `original_destination`;
    ///
    /// 2. Sends the [`Request`] to it, and awaits a [`Response`];
    ///
    /// 3. Sends the [`HttpResponse`] back on the connected [`TcpStream`];
    #[tracing::instrument(level = "trace")]
    async fn unmatched_request(
        request: Request<Incoming>,
        upgrade_tx: Option<oneshot::Sender<RawHyperConnection>>,
        original_destination: SocketAddr,
    ) -> Result<Response, HttpTrafficError> {
        // TODO(alex): We need a "retry" mechanism here for the client handling part, when the
        // server closes a connection, the client could still be wanting to send a request,
        // so we need to re-connect and send.
        let tcp_stream = TcpStream::connect(original_destination)
            .await
            .inspect_err(|fail| {
                error!("Failed connecting to original_destination with {fail:#?}")
            })?;

        let mut request_sender = V::connect(tcp_stream, upgrade_tx)
            .await
            .inspect_err(|fail| error!("Handshake failed with {fail:#?}"))?;

        V::send_request(&mut request_sender, request).await
    }

    /// Handles the incoming HTTP/V [`Request`].
    ///
    /// Checks if this [`Request`] contains an upgrade header, and if not, then checks if a header
    /// matches one of the user specified filters.
    ///
    /// Helper function due to the fact that [`Service::call`] is not an `async` function.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn handle_request(
        request: Request<Incoming>,
        original_destination: SocketAddr,
        upgrade_tx: Option<oneshot::Sender<RawHyperConnection>>,
        filters: Arc<DashMap<ClientId, HttpFilter>>,
        port: Port,
        connection_id: ConnectionId,
        request_id: RequestId,
        matched_tx: Sender<HandlerHttpRequest>,
    ) -> Result<Response, HttpTrafficError> {
        if V::is_upgrade(&request) {
            Self::unmatched_request(request, upgrade_tx, original_destination).await
        } else if let Some(client_id) = match_request(&request, &filters) {
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
