use core::fmt::Debug;
use std::{net::SocketAddr, sync::Arc};

use bytes::Bytes;
use dashmap::DashMap;
use fancy_regex::Regex;
use futures::TryFutureExt;
use http_body_util::{BodyExt, Full};
use hyper::{
    body::Incoming,
    http::{self, response},
    Request, Response,
};
use mirrord_protocol::{ConnectionId, Port, RequestId};
use tokio::{
    net::TcpStream,
    sync::{mpsc::Sender, oneshot::Receiver},
};

use super::error::HttpTrafficError;
use crate::{steal::HandlerHttpRequest, util::ClientId};

pub(super) mod httpv1;
pub(super) mod httpv2;

/// Used to pass data to the [`Service`] implementation of [`hyper`].
///
/// Think of this struct as if it was a bunch of function arguments that are passed down to
/// [`Service::call`] method.
///
/// Each [`TcpStream`] connection (for stealer) requires this.
#[derive(Debug)]
pub(super) struct HyperHandler<HandleVersion>
where
    HandleVersion: Debug,
{
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

    pub(super) handle_version: HandleVersion,
}

/// Holds a connection and bytes that were left unprocessed by the [`hyper`] state machine.
#[derive(Debug)]
pub(super) struct RawHyperConnection {
    /// The connection as a raw IO object.
    pub(super) stream: TcpStream,

    /// Bytes that were not handled by [`hyper`].
    pub(super) unprocessed_bytes: Bytes,
}

fn header_matches(
    request: &Request<Incoming>,
    filters: &Arc<DashMap<ClientId, Regex>>,
) -> Option<ClientId> {
    request
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
}

async fn prepare_response(
    (mut parts, body): (response::Parts, Incoming),
) -> Result<Response<Full<Bytes>>, HttpTrafficError> {
    // Remove headers that would be invalid due to us fiddling with the `body`.
    let body = body.collect().await?.to_bytes();
    parts.headers.remove(http::header::CONTENT_LENGTH);
    parts.headers.remove(http::header::TRANSFER_ENCODING);

    // Rebuild the `Response` after our fiddling.
    Ok(Response::from_parts(parts, body.into()))
}

/// Sends a [`MatchedHttpRequest`] through `tx` to be handled by the stealer -> layer,
/// and then waits for the response and returns it once it's there.
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
