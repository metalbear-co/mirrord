use std::error::Report;

use futures::TryFutureExt;
use http_body_util::combinators::BoxBody;
use hyper::{
    http::{request, StatusCode, Version},
    Request,
};
use hyper_util::rt::TokioIo;
use mirrord_tls_util::MaybeTls;
use tokio::net::TcpStream;

use super::{ConnectionInfo, IncomingIO};
use crate::{
    http::{
        body::RolledBackBody, error::MirrordErrorResponse, extract_requests::ExtractedRequest,
        sender::HttpSender, HttpVersion,
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
    pub async fn run(self, extracted: ExtractedRequest<TokioIo<Box<dyn IncomingIO>>>) {
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

    async fn run_inner(
        &self,
        extracted: ExtractedRequest<TokioIo<Box<dyn IncomingIO>>>,
    ) -> Result<(), ConnError> {
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

        let body = RolledBackBody {
            head: extracted.body_head.into_iter(),
            tail: extracted.body_tail,
        };
        let incoming_upgrade = extracted.upgrade;
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
                .map_err(ConnError::PassthroughHttpError),
            incoming_upgrade
                .into_inner()
                .map_ok(TokioIo::new)
                .map_err(ConnError::IncomingHttpError),
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
    ) -> Result<HttpSender<RolledBackBody>, ConnError> {
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
