use std::{
    error::Report,
    pin::Pin,
    task::{self, Context, Poll},
    vec,
};

use bytes::{Bytes, BytesMut};
use futures::TryFutureExt;
use hyper::{
    body::{Body, Frame, Incoming},
    client::conn::{http1, http2},
    http::{request, StatusCode, Version},
    upgrade::Upgraded,
    Request,
};
use hyper_util::rt::{TokioExecutor, TokioIo};
use mirrord_tls_util::MaybeTls;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

use super::{error_response::MirrordErrorResponse, service::ExtractedRequest, BoxBody};
use crate::{
    http::HttpSender,
    incoming::{
        connection::{util::AutoDropBroadcast, ConnectionInfo, IncomingIO},
        error::{ConnError, ResultExt},
        IncomingStreamItem,
    },
};

/// Background task responsible for handling IO on redirected HTTP request,
/// that is not stolen.
pub struct PassThroughTask {
    pub info: ConnectionInfo,
    pub copy_tx: AutoDropBroadcast<IncomingStreamItem>,
}

impl PassThroughTask {
    /// Runs this task until the request is finished.
    ///
    /// This method must ensure that the final [`IncomingStreamItem::Finished`] is always sent to
    /// the clients.
    pub async fn run(mut self, request: ExtractedRequest) {
        let result = self.run_inner(request).await;
        self.copy_tx.send(IncomingStreamItem::Finished(result));
    }

    pub async fn run_inner(&mut self, extracted: ExtractedRequest) -> Result<(), ConnError> {
        let version = extracted.parts.version;
        let uri = extracted.parts.uri.clone();
        let method = extracted.parts.method.clone();

        let mut sender = match self.make_connection(&extracted.parts).await {
            Ok(sender) => sender,
            Err(error) => {
                tracing::warn!(
                    error = %Report::new(&error),
                    %uri, %method, ?version,
                    info = ?self.info,
                    "Failed to make an HTTP connection to the original destination",
                );

                let _ = extracted.response_tx.send(
                    MirrordErrorResponse::new(version, Report::new(&error).pretty(true)).into(),
                );
                return Err(error);
            }
        };

        let body = PassThroughBody {
            head: extracted.body_head.into_iter(),
            tail: extracted.body_tail,
            copy_tx: self.copy_tx.clone(),
        };
        let incoming_upgrade = extracted.on_upgrade;
        let request = Request::from_parts(extracted.parts, body);
        let mut response = match sender.send(request).await {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!(
                    error = %Report::new(&error),
                    %uri, %method, ?version,
                    info = ?self.info,
                    "Failed to send an HTTP request to the original destination",
                );

                let error = ConnError::PassthroughHttpError(error.into());
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

        let (upgraded_outgoing, upgraded_incoming) = tokio::try_join!(
            outgoing_upgrade.map_err(|error| ConnError::PassthroughHttpError(error.into())),
            incoming_upgrade.map_err(|error| ConnError::IncomingHttpError(error.into())),
        )?;

        self.proxy_upgraded_data(upgraded_outgoing, upgraded_incoming)
            .await?;

        Ok(())
    }

    /// Handles bidirectional data transfer after an HTTP upgrade.
    async fn proxy_upgraded_data(
        &mut self,
        upgraded_outgoing: Upgraded,
        upgraded_incoming: Upgraded,
    ) -> Result<(), ConnError> {
        let parts_outgoing = upgraded_outgoing
            .downcast::<TokioIo<MaybeTls>>()
            .expect("io type is known");
        let mut outgoing = parts_outgoing.io.into_inner();

        let parts_incoming = upgraded_incoming
            .downcast::<TokioIo<Box<dyn IncomingIO>>>()
            .expect("io type is known");
        let mut incoming = parts_incoming.io.into_inner();

        tokio::try_join!(
            async {
                if parts_outgoing.read_buf.is_empty() {
                    return Ok(());
                }

                incoming
                    .write_all(&parts_outgoing.read_buf)
                    .await
                    .map_err_into(ConnError::IncomingUpgradedError)
            },
            async {
                if parts_incoming.read_buf.is_empty() {
                    return Ok(());
                }

                self.copy_tx.send(parts_incoming.read_buf.as_ref());

                outgoing
                    .write_all(&parts_incoming.read_buf)
                    .await
                    .map_err_into(ConnError::PassthroughUpgradedError)
            },
        )?;

        let mut buff_outgoing = BytesMut::with_capacity(64 * 1024);
        let mut outgoing_writes = true;
        let mut buff_incoming = BytesMut::with_capacity(64 * 1024);
        let mut incoming_writes = true;

        while outgoing_writes || incoming_writes {
            tokio::select! {
                result = outgoing.read_buf(&mut buff_outgoing), if outgoing_writes => {
                    result.map_err_into(ConnError::PassthroughUpgradedError)?;

                    if buff_outgoing.is_empty() {
                        outgoing_writes = false;
                        incoming.shutdown()
                            .await
                            .map_err_into(ConnError::IncomingUpgradedError)?;
                    } else {
                        incoming.write_all(&buff_outgoing)
                            .await
                            .map_err_into(ConnError::IncomingUpgradedError)?;
                        buff_outgoing.clear();
                    }
                },

                result = incoming.read_buf(&mut buff_incoming), if incoming_writes => {
                    result.map_err_into(ConnError::IncomingUpgradedError)?;

                    if buff_incoming.is_empty() {
                        incoming_writes = false;
                        outgoing.shutdown().await.map_err_into(ConnError::PassthroughUpgradedError)?;
                        self.copy_tx.send(IncomingStreamItem::NoMoreData);
                    } else {
                        self.copy_tx.send(buff_incoming.as_ref());
                        outgoing.write_all(&buff_incoming).await.map_err_into(ConnError::PassthroughUpgradedError)?;
                        buff_incoming.clear();
                    }
                },
            }
        }

        Ok(())
    }

    /// Makes an HTTP connection to the original destination.
    async fn make_connection(
        &self,
        extracted: &request::Parts,
    ) -> Result<HttpSender<PassThroughBody>, ConnError> {
        let stream = TcpStream::connect(self.info.pass_through_address())
            .await
            .map_err_into(ConnError::TcpConnectError)?;

        let stream = match &self.info.tls_connector {
            Some(connector) => {
                let stream = connector
                    .connect(
                        self.info.original_destination.ip(),
                        Some(&extracted.uri),
                        stream,
                    )
                    .await
                    .map_err_into(ConnError::TcpConnectError)?;
                MaybeTls::Tls(stream)
            }
            None => MaybeTls::NoTls(stream),
        };

        match extracted.version {
            Version::HTTP_2 => {
                let (sender, conn) = http2::handshake::<_, _, PassThroughBody>(
                    TokioExecutor::default(),
                    TokioIo::new(stream),
                )
                .await
                .map_err_into(ConnError::PassthroughHttpError)?;

                tokio::spawn(conn);
                Ok(HttpSender::V2(sender))
            }
            _ => {
                let (sender, conn) = http1::handshake::<_, PassThroughBody>(TokioIo::new(stream))
                    .await
                    .map_err_into(ConnError::PassthroughHttpError)?;

                tokio::spawn(conn.with_upgrades());
                Ok(HttpSender::V1(sender))
            }
        }
    }
}

/// [`Body`] type used when passing the redirected [`Request`] to its original destination.
///
/// [`Body::poll_frame`] automatically sends the frames to the mirroring clients.
struct PassThroughBody {
    head: vec::IntoIter<Frame<Bytes>>,
    tail: Option<Incoming>,
    copy_tx: AutoDropBroadcast<IncomingStreamItem>,
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

        match task::ready!(Pin::new(tail).poll_frame(cx)) {
            Some(Ok(frame)) => {
                this.copy_tx.send(&frame);
                Poll::Ready(Some(Ok(frame)))
            }
            Some(Err(error)) => {
                // We don't send the error through the broadcast channel,
                // because `PassThroughTask::run` will do it.
                this.tail = None;
                Poll::Ready(Some(Err(error)))
            }
            None => {
                this.tail = None;
                this.copy_tx.send(IncomingStreamItem::NoMoreFrames);
                Poll::Ready(None)
            }
        }
    }
}
