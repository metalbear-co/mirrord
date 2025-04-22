use std::{
    pin::Pin,
    sync::Arc,
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
    Request, Response,
};
use hyper_util::rt::{TokioExecutor, TokioIo};
use mirrord_tls_util::MaybeTls;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

use super::{error_response::MirrordErrorResponse, service::ExtractedRequest, BoxBody};
use crate::incoming::{
    connection::{util::AutoDropBroadcast, ConnectionInfo, IncomingIO},
    error::{ConnError, ResultExt},
    IncomingStreamItem,
};

pub struct PassThroughTask {
    pub info: ConnectionInfo,
    pub copy_tx: AutoDropBroadcast<IncomingStreamItem>,
}

impl PassThroughTask {
    pub async fn run(mut self, request: ExtractedRequest) {
        let conn_status = request.status.clone();

        let result = tokio::select! {
            result = self.run_inner(request) => result,
            result = conn_status => result,
        };

        self.copy_tx.send(IncomingStreamItem::Finished(result));
    }

    pub async fn run_inner(&mut self, extracted: ExtractedRequest) -> Result<(), ConnError> {
        let version = extracted.parts.version;

        let mut sender = match self.make_connection(&extracted.parts).await {
            Ok(sender) => sender,
            Err(error) => {
                let _ = extracted
                    .response_tx
                    .send(MirrordErrorResponse::new(version, &error).into());
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
                let error = ConnError::PassthroughHttpError(error.into());
                let _ = extracted
                    .response_tx
                    .send(MirrordErrorResponse::new(version, &error).into());
                return Err(error);
            }
        };

        let outgoing_upgrade = (response.status() == StatusCode::SWITCHING_PROTOCOLS)
            .then(|| hyper::upgrade::on(&mut response));

        if extracted
            .response_tx
            .send(response.map(BoxBody::new))
            .is_err()
        {
            return extracted.status.clone().await;
        }

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

    async fn make_connection(&self, extracted: &request::Parts) -> Result<Sender, ConnError> {
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
                Ok(Sender::Http2(sender))
            }
            _ => {
                let (sender, conn) = http1::handshake::<_, PassThroughBody>(TokioIo::new(stream))
                    .await
                    .map_err_into(ConnError::PassthroughHttpError)?;

                tokio::spawn(conn.with_upgrades());
                Ok(Sender::Http1(sender))
            }
        }
    }
}

enum Sender {
    Http1(http1::SendRequest<PassThroughBody>),
    Http2(http2::SendRequest<PassThroughBody>),
}

impl Sender {
    async fn send(
        &mut self,
        request: Request<PassThroughBody>,
    ) -> hyper::Result<Response<Incoming>> {
        match self {
            Self::Http1(sender) => sender.send_request(request).await,
            Self::Http2(sender) => sender.send_request(request).await,
        }
    }
}

struct PassThroughBody {
    head: vec::IntoIter<Frame<Bytes>>,
    tail: Option<Incoming>,
    copy_tx: AutoDropBroadcast<IncomingStreamItem>,
}

impl Body for PassThroughBody {
    type Data = Bytes;
    type Error = Arc<hyper::Error>;

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
                this.tail = None;
                let error = Arc::new(error);
                this.copy_tx.send(IncomingStreamItem::Finished(Err(
                    ConnError::IncomingHttpError(error.clone()),
                )));
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
