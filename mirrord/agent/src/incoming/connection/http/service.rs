use std::{future::Future, ops::Not, pin::Pin};

use bytes::Bytes;
use futures::{future::Either, FutureExt};
use http::request::Parts;
use hyper::{
    body::{Frame, Incoming},
    server::conn::{http1, http2},
    service::Service,
    upgrade::OnUpgrade,
    Error, Request,
};
use hyper_util::rt::{TokioExecutor, TokioIo};
use mirrord_protocol::batched_body::{BatchedBody, Frames};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::WaitForCancellationFutureOwned;

use super::{error_response::MirrordErrorResponse, BoxResponse};
use crate::{
    http::HttpVersion,
    incoming::{
        connection::{ConnectionInfo, IncomingIO},
        error::{ConnError, ResultExt},
        task::InternalMessage,
    },
    util::status::{self, StatusReceiver},
};

pub struct ExtractedRequest {
    pub parts: Parts,
    pub body_head: Vec<Frame<Bytes>>,
    pub body_tail: Option<Incoming>,
    pub on_upgrade: OnUpgrade,
    pub response_tx: oneshot::Sender<BoxResponse>,
    pub status: StatusReceiver<Result<(), ConnError>>,
}

#[derive(Clone)]
struct RequestExtractor {
    connection_info: ConnectionInfo,
    status: StatusReceiver<Result<(), ConnError>>,
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
                    status: this.status,
                },
                this.connection_info,
            );

            if this.request_tx.send(message).await.is_err() {
                return Ok(MirrordErrorResponse::new(version, "request dropped").into());
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

pub async fn extract_requests(
    connection_info: ConnectionInfo,
    request_tx: mpsc::Sender<InternalMessage>,
    stream: Box<dyn IncomingIO>,
    version: HttpVersion,
    shutdown: WaitForCancellationFutureOwned,
) {
    let (status_tx, status) = status::channel();

    let extractor = RequestExtractor {
        connection_info,
        status,
        request_tx,
    };

    let mut connection = match version {
        HttpVersion::V1 => {
            let conn = http1::Builder::new()
                .preserve_header_case(true)
                .serve_connection(TokioIo::new(stream), extractor)
                .with_upgrades();
            Either::Left(conn)
        }

        HttpVersion::V2 => {
            let conn = http2::Builder::new(TokioExecutor::default())
                .serve_connection(TokioIo::new(stream), extractor);
            Either::Right(conn)
        }
    };

    let result = tokio::select! {
        result = &mut connection => result,
        _ = shutdown => {
            match &mut connection {
                Either::Left(conn) => {
                    Pin::new(conn).graceful_shutdown();
                }
                Either::Right(conn) => {
                    Pin::new(conn).graceful_shutdown();
                }
            }

            connection.await
        }
    };

    let result = result.map_err_into(ConnError::IncomingHttpError);
    status_tx.send(result);
}
