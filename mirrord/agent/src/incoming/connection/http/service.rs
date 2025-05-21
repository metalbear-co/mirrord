use std::{future::Future, ops::Not, pin::Pin};

use bytes::Bytes;
use futures::FutureExt;
use http::request::Parts;
use hyper::{
    body::{Frame, Incoming},
    service::Service,
    upgrade::OnUpgrade,
    Error, Request,
};
use mirrord_protocol::batched_body::{BatchedBody, Frames};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::WaitForCancellationFutureOwned;

use super::{error_response::MirrordErrorResponse, BoxResponse};
use crate::{
    http::HttpVersion,
    incoming::{
        connection::{ConnectionInfo, IncomingIO},
        task::InternalMessage,
    },
};

/// An HTTP request extracted from a redirected incoming connection.
pub struct ExtractedRequest {
    pub parts: Parts,
    pub body_head: Vec<Frame<Bytes>>,
    pub body_tail: Option<Incoming>,
    pub on_upgrade: OnUpgrade,
    /// Can be used send the response back to the remote HTTP client.
    ///
    /// Try not to drop it without providing a meaningful error response
    /// ([`super::error_response::MirrordErrorResponse`]). Providing meaningful error responses
    /// helps when debugging user issues.
    pub response_tx: oneshot::Sender<BoxResponse>,
}

/// Implementation of [`Service`] that sends [`Request`]s through the inner [`mpsc::Sender`]
/// and returns responses produced somewhere else.
///
/// The responses are received from [`oneshot::Receiver`] send with the [`Request`] in an
/// [`InternalMessage`].
#[derive(Clone)]
struct RequestExtractor {
    connection_info: ConnectionInfo,
    request_tx: mpsc::Sender<InternalMessage>,
}

impl Service<Request<Incoming>> for RequestExtractor {
    type Response = BoxResponse;
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn call(&self, mut request: Request<Incoming>) -> Self::Future {
        let this = self.clone();

        async move {
            let (response_tx, response_rx) = oneshot::channel();
            let version = request.version();
            let on_upgrade = hyper::upgrade::on(&mut request);
            let (parts, mut body) = request.into_parts();
            let Frames { frames, is_last } = body.ready_frames()?;
            let body_head = frames;
            let body_tail = is_last.not().then_some(body);

            let message = InternalMessage::Request(
                ExtractedRequest {
                    parts,
                    body_head,
                    body_tail,
                    on_upgrade,
                    response_tx,
                },
                this.connection_info,
            );

            if this.request_tx.send(message).await.is_err() {
                return Ok(MirrordErrorResponse::new(
                    version,
                    "redirector task is exiting, request dropped",
                )
                .into());
            }

            let response = match response_rx.await {
                Ok(response) => response,
                Err(..) => MirrordErrorResponse::new(version, "request dropped").into(),
            };

            Ok(response)
        }
        .boxed()
    }
}

/// Background task for a redirected HTTP connection.
///
/// Used by [`RedirectorTask`](crate::incoming::RedirectorTask) to:
/// 1. Handle IO.
/// 2. Extract HTTP requests.
///
/// The requests are sent through the given [`mpsc::Sender`].
///
/// The given `shutdown` can be used to gracefully shutdown the HTTP server.
pub async fn extract_requests(
    connection_info: ConnectionInfo,
    request_tx: mpsc::Sender<InternalMessage>,
    stream: Box<dyn IncomingIO>,
    version: HttpVersion,
    shutdown: WaitForCancellationFutureOwned,
) {
    let extractor = RequestExtractor {
        connection_info: connection_info.clone(),
        request_tx,
    };

    let result = crate::http::run_http_server(stream, extractor, version, shutdown).await;
    if let Err(error) = result {
        tracing::warn!(
            %error,
            ?connection_info,
            "Incoming HTTP connection failed",
        )
    }
}
