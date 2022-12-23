use core::{future::Future, pin::Pin};
use std::{net::SocketAddr, sync::Arc};

use dashmap::DashMap;
use fancy_regex::Regex;
use futures::TryFutureExt;
use hyper::{body::Incoming, client, service::Service, Request, Response};
use mirrord_protocol::{tcp::HttpResponse, ConnectionId, Port, RequestId};
use tokio::{net::TcpStream, sync::mpsc::Sender};
use tracing::trace;

use super::{error::HttpTrafficError, UnmatchedHttpResponse, UnmatchedSender};
use crate::{steal::MatchedHttpRequest, util::ClientId};

pub(super) const DUMMY_RESPONSE_MATCHED: &str = "Matched!";
pub(super) const DUMMY_RESPONSE_UNMATCHED: &str = "Unmatched!";

#[derive(Debug)]
pub(super) struct HyperHandler {
    pub(super) filters: Arc<DashMap<ClientId, Regex>>,
    pub(super) matched_tx: Sender<MatchedHttpRequest>,
    pub(super) unmatched_tx: UnmatchedSender,
    pub(crate) connection_id: ConnectionId,
    pub(crate) port: Port,
    pub(crate) original_destination: SocketAddr,
    pub(crate) request_id: RequestId,
}

/// Handles the case when no filter matches a header in the request.
///
/// 1. Creates a [`hyper::client::conn::http1::Connection`] to the `original_destination`;
/// 2. Sends the [`Request`] to it, and awaits a [`Response`];
/// 3. Sends the [`HttpResponse`] to the stealer, via the [`UnmatchedSender`] channel.
#[tracing::instrument(level = "debug", skip(tx))]
fn unmatched_request(
    request: Request<Incoming>,
    tx: UnmatchedSender,
    original_destination: SocketAddr,
    connection_id: ConnectionId,
    request_id: RequestId,
) {
    // TODO(alex): We need a "retry" mechanism here for the client handling part, when the server
    // closes a connection, the client could still be wanting to send a request, so we need to
    // re-connect and send.
    tokio::spawn(async move {
        let response = TcpStream::connect(original_destination)
            .map_err(From::from)
            .and_then(|target_stream| {
                client::conn::http1::handshake(target_stream).map_err(From::from)
            })
            .and_then(|(mut request_sender, connection)| {
                let tx = tx.clone();

                tokio::spawn(async move {
                    if let Err(fail) = connection.await {
                        let _ = tx.send(Err(fail.into())).await.inspect_err(|fail| {
                            trace!("Sending an `UnmatchedHttpResponse` failed due to {fail:#?}!")
                        });
                    }
                });

                request_sender.send_request(request).map_err(From::from)
            })
            .and_then(|intercepted_response| {
                HttpResponse::from_hyper_response(
                    intercepted_response,
                    original_destination.port(),
                    connection_id,
                    request_id,
                )
                .map_err(From::from)
            })
            .await
            .map(UnmatchedHttpResponse);

        tx.send(response).await.inspect_err(|fail| {
            trace!("Sending an `UnmatchedHttpResponse` failed due to {fail:#?}!")
        })
    });
}

impl Service<Request<Incoming>> for HyperHandler {
    type Response = Response<String>;

    type Error = HttpTrafficError;

    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    // TODO(alex) [mid] 2022-12-13: Do we care at all about what is sent from here as a response to
    // our client duplex stream?
    #[tracing::instrument(level = "debug", skip(self))]
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
            let request = MatchedHttpRequest {
                port: self.port,
                connection_id: self.connection_id,
                client_id,
                request_id: self.request_id,
                request,
            };

            let matched_tx = self.matched_tx.clone();

            // Creates a task to send the matched request (cannot use `await` in the `call`
            // function, so we have to do this).
            tokio::spawn(async move {
                matched_tx
                    .send(request)
                    .map_err(HttpTrafficError::from)
                    .await
            });

            self.request_id += 1;

            let response = async { Ok(Response::new(DUMMY_RESPONSE_MATCHED.to_string())) };
            Box::pin(response)
        } else {
            unmatched_request(
                request,
                self.unmatched_tx.clone(),
                self.original_destination,
                self.connection_id,
                self.request_id,
            );
            self.request_id += 1;

            let response = async { Ok(Response::new(DUMMY_RESPONSE_UNMATCHED.to_string())) };
            Box::pin(response)
        }
    }
}
