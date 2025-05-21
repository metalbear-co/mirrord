use std::{
    error::Report,
    pin::Pin,
    task::{Context, Poll},
    vec,
};

use bytes::Bytes;
use futures::TryFutureExt;
use http_body_util::combinators::BoxBody;
use hyper::{
    body::{Body, Frame, Incoming},
    http::{request, StatusCode, Version},
    Request,
};
use hyper_util::rt::TokioIo;
use mirrord_tls_util::MaybeTls;
use tokio::net::TcpStream;

use super::ConnectionInfo;
use crate::{
    http::{
        error::MirrordErrorResponse, extract_requests::ExtractedRequest, sender::HttpSender,
        HttpVersion,
    },
    incoming::error::ConnError,
};

/// Background task responsible for handling IO on a redirected HTTP request
/// that was not stolen.
pub struct PassThroughTask {
    pub info: ConnectionInfo,
}

impl PassThroughTask {
    /// Runs this task until the request is finished.
    pub async fn run(self, extracted: ExtractedRequest) {
        let uri = extracted.parts.uri.clone();
        let method = extracted.parts.method.clone();

        if let Err(error) = self.run_inner(extracted).await {
            tracing::warn!(
                %uri,
                %method,
                error = %Report::new(error),
                "Failed to pass through an unstolen HTTP request",
            )
        }
    }

    async fn run_inner(&self, extracted: ExtractedRequest) -> Result<(), ConnError> {
        let version = extracted.parts.version;

        let mut sender = match self.make_connection(&extracted.parts).await {
            Ok(sender) => sender,
            Err(error) => {
                let _ = extracted.response_tx.send(
                    MirrordErrorResponse::new(version, Report::new(&error).pretty(true)).into(),
                );
                return Err(error);
            }
        };

        let body = PassThroughBody {
            head: extracted.body_head.into_iter(),
            tail: extracted.body_tail,
        };
        let incoming_upgrade = extracted.on_upgrade;
        let mut response = match sender
            .send(Request::from_parts(extracted.parts, body))
            .await
        {
            Ok(response) => response,
            Err(error) => {
                let error = ConnError::PassthroughHttpError(error);
                let _ = extracted.response_tx.send(
                    MirrordErrorResponse::new(version, Report::new(&error).pretty(true)).into(),
                );
                return Err(error);
            }
        };

        let outgoing_upgrade = (response.status() == StatusCode::SWITCHING_PROTOCOLS)
            .then(|| hyper::upgrade::on(&mut response));

        let _ = extracted.response_tx.send(response.map(BoxBody::new));

        let Some(outgoing_upgrade) = outgoing_upgrade else {
            return Ok(());
        };

        let (mut outgoing, mut incoming) = tokio::try_join!(
            outgoing_upgrade
                .map_ok(TokioIo::new)
                .map_err(|error| ConnError::PassthroughHttpError(error.into())),
            incoming_upgrade
                .map_ok(TokioIo::new)
                .map_err(|error| ConnError::IncomingHttpError(error.into())),
        )?;

        tokio::io::copy_bidirectional(&mut outgoing, &mut incoming)
            .await
            .map_err(ConnError::UpgradedError)?;

        Ok(())
    }

    /// Makes an HTTP connection to the original destination.
    async fn make_connection(
        &self,
        extracted: &request::Parts,
    ) -> Result<HttpSender<PassThroughBody>, ConnError> {
        let stream = TcpStream::connect(self.info.pass_through_address())
            .await
            .map_err(ConnError::TcpConnectError)?;

        let stream = match &self.info.tls_connector {
            Some(connector) => {
                let stream = connector
                    .connect(
                        self.info.original_destination.ip(),
                        Some(&extracted.uri),
                        stream,
                    )
                    .await
                    .map_err(ConnError::TcpConnectError)?;
                MaybeTls::Tls(stream)
            }
            None => MaybeTls::NoTls(stream),
        };

        let version = match extracted.version {
            Version::HTTP_2 => HttpVersion::V2,
            _ => HttpVersion::V1,
        };

        HttpSender::new(TokioIo::new(stream), version)
            .await
            .map_err(ConnError::PassthroughHttpError)
    }
}

/// [`Body`] type used when passing the redirected [`Request`] to its original destination.
///
/// Contains some first frames that were eagerly read,
/// and the optional rest of the body.
struct PassThroughBody {
    head: vec::IntoIter<Frame<Bytes>>,
    tail: Option<Incoming>,
}

impl Body for PassThroughBody {
    type Data = Bytes;
    type Error = hyper::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();

        if let Some(frame) = this.head.next() {
            return Poll::Ready(Some(Ok(frame)));
        }

        let Some(tail) = this.tail.as_mut() else {
            return Poll::Ready(None);
        };

        Pin::new(tail).poll_frame(cx)
    }
}
