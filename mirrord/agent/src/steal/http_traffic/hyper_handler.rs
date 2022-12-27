use core::{future::Future, pin::Pin};
use std::{net::SocketAddr, sync::Arc};

use bytes::Bytes;
use dashmap::DashMap;
use fancy_regex::Regex;
use futures::TryFutureExt;
use http_body_util::{combinators::BoxBody, BodyExt, Full};
use hyper::{
    body::{Body, Incoming},
    client, http,
    service::Service,
    Request, Response,
};
use mirrord_protocol::{tcp::HttpResponse, ConnectionId, Port, RequestId};
use tokio::{
    net::TcpStream,
    sync::{
        mpsc::{error::SendError, Sender},
        oneshot::{self, error::RecvError, Receiver},
    },
};
use tracing::trace;

use super::{error::HttpTrafficError, UnmatchedHttpResponse, UnmatchedSender};
use crate::{
    error::AgentError,
    steal::{HandlerHttpRequest, MatchedHttpRequest},
    util::ClientId,
};

pub(super) const DUMMY_RESPONSE_MATCHED: &str = "Matched!";
pub(super) const DUMMY_RESPONSE_UNMATCHED: &str = "Unmatched!";

#[derive(Debug)]
pub(super) struct HyperHandler {
    pub(super) filters: Arc<DashMap<ClientId, Regex>>,
    pub(super) matched_tx: Sender<HandlerHttpRequest>,
    pub(super) unmatched_tx: UnmatchedSender,
    pub(crate) connection_id: ConnectionId,
    pub(crate) port: Port,
    pub(crate) original_destination: SocketAddr,
    pub(crate) request_id: RequestId,
}

/// Sends a [`MatchedHttpRequest`] through `tx` to be handled by the stealer -> layer.
#[tracing::instrument(level = "debug", skip(matched_tx, response_rx))]
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
    parts.headers.remove(http::header::CONTENT_ENCODING);

    Ok(Response::from_parts(parts, body))
}

/// Handles the case when no filter matches a header in the request.
///
/// 1. Creates a [`hyper::client::conn::http1::Connection`] to the `original_destination`;
/// 2. Sends the [`Request`] to it, and awaits a [`Response`];
/// 3. Sends the [`HttpResponse`] to the stealer, via the [`UnmatchedSender`] channel.
#[tracing::instrument(level = "debug")]
async fn unmatched_request(
    request: Request<Incoming>,
    original_destination: SocketAddr,
) -> Result<Response<Full<Bytes>>, HttpTrafficError> {
    // TODO(alex): We need a "retry" mechanism here for the client handling part, when the server
    // closes a connection, the client could still be wanting to send a request, so we need to
    // re-connect and send.
    let tcp_stream = TcpStream::connect(original_destination).await?;
    let (mut request_sender, connection) = client::conn::http1::handshake(tcp_stream).await?;

    tokio::spawn(async move {
        if let Err(fail) = connection.await {
            trace!("HTTP connection for unmatched request failed: {fail:#?}!");
        }
    });

    let (mut parts, body) = request_sender.send_request(request).await?.into_parts();
    let body = body.collect().await?.to_bytes();

    parts.headers.remove(http::header::CONTENT_LENGTH);
    parts.headers.remove(http::header::CONTENT_ENCODING);

    Ok(Response::from_parts(parts, body.into()))
}

impl Service<Request<Incoming>> for HyperHandler {
    type Response = Response<Full<Bytes>>;

    type Error = HttpTrafficError;

    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    // TODO(alex) [mid] 2022-12-13: Do we care at all about what is sent from here as a response to
    // our client duplex stream?
    // #[tracing::instrument(level = "debug", skip(self))]
    fn call(&mut self, request: Request<Incoming>) -> Self::Future {
        if let Some(client_id) = request
            .headers()
            .iter()
            .map(|(header_name, header_value)| {
                header_value
                    .to_str()
                    .map(|header_value| format!("{}={}", header_name, header_value))
            })
            .find_map(|header| {
                self.filters.iter().find_map(|filter| {
                    // TODO(alex) [low] 2022-12-23: Remove the `header` unwrap.
                    if filter.is_match(header.as_ref().unwrap()).unwrap() {
                        Some(*filter.key())
                    } else {
                        None
                    }
                })
            })
        {
            self.request_id += 1;
            let req = MatchedHttpRequest {
                port: self.port,
                connection_id: self.connection_id,
                client_id,
                request_id: self.request_id,
                request,
            };

            let (response_tx, response_rx) = oneshot::channel();
            let handler_request = HandlerHttpRequest {
                request: req,
                response_tx,
            };

            Box::pin(matched_request(
                handler_request,
                self.matched_tx.clone(),
                response_rx,
            ))
        } else {
            self.request_id += 1;

            Box::pin(unmatched_request(request, self.original_destination))
        }
    }
}
