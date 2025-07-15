use std::{error::Report, future::Future};

use bytes::{Bytes, BytesMut};
use futures::StreamExt;
use http_body_util::{combinators::BoxBody, BodyExt, StreamBody};
use hyper::{
    body::{Body, Frame, Incoming},
    http::{StatusCode, Version},
    upgrade::Upgraded,
    Request, Response,
};
use hyper_util::rt::TokioIo;
use mirrord_protocol::{tcp::InternalHttpBodyFrame, Payload};
use mirrord_tls_util::MaybeTls;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::{broadcast, mpsc, oneshot},
    task::JoinHandle,
};
use tokio_stream::wrappers::ReceiverStream;

use crate::{
    http::{
        body::RolledBackBody,
        error::MirrordErrorResponse,
        extract_requests::{ExtractedRequest, HttpUpgrade},
        sender::HttpSender,
        HttpVersion,
    },
    incoming::{
        connection::{
            copy_bidirectional::{self, CowBytes, OutgoingDestination},
            ConnectionInfo, IncomingIO,
        },
        error::ConnError,
        IncomingStreamItem,
    },
};

pub type UpgradeDataRx = mpsc::Receiver<Bytes>;

/// Background task responsible for handling *some* of the IO on a redirected HTTP request.
///
/// This task handles:
/// 1. Reading incoming request body frames from [`Self::body_tail`] and passing them to
///    [`Self::destination`].
/// 2. Passing data in both directions after an HTTP upgrade from [`Self::destination`].
pub struct HttpTask<D> {
    /// Frames that we need to send to the request destination.
    pub body_tail: Option<Incoming>,
    /// Extracted from the original request.
    pub on_upgrade: HttpUpgrade<TokioIo<Box<dyn IncomingIO>>>,
    /// Destination of the request.
    pub destination: D,
}

impl<D> HttpTask<D>
where
    D: RequestDestination,
{
    /// Runs this task until the request is finished.
    ///
    /// This method must ensure that the final [`IncomingStreamItem::Finished`] is always sent to
    /// the client.
    pub async fn run(mut self) {
        let result: Result<(), ConnError> = try {
            self.handle_frames().await?;
            self.handle_upgrade().await?;
        };

        self.destination.send_result(result).await;
    }

    /// Handles reading request body frames.
    ///
    /// In this task, we only handle body tail, i.e. frames that were not initially ready.
    async fn handle_frames(&mut self) -> Result<(), ConnError> {
        let Some(mut body) = self.body_tail.take() else {
            return Ok(());
        };

        while let Some(result) = body.frame().await {
            let frame = result
                .map_err(From::from)
                .map_err(ConnError::IncomingHttpError)?;

            self.destination.send_frame(frame).await?;
        }

        self.destination.no_more_frames().await?;

        Ok(())
    }

    /// Handles bidirectional data transfer after an HTTP upgrade.
    async fn handle_upgrade(&mut self) -> Result<(), ConnError> {
        let Some(mut upgraded_destination) = self.destination.wait_for_upgrade().await? else {
            return Ok(());
        };

        let (upgraded_peer, read_buf) = (&mut self.on_upgrade)
            .await
            .map_err(From::from)
            .map_err(ConnError::IncomingHttpError)?;
        let mut upgraded_peer = upgraded_peer.into_inner();

        copy_bidirectional::copy_bidirectional(
            &mut upgraded_peer,
            &mut upgraded_destination,
            read_buf,
        )
        .await
    }
}

impl HttpTask<PassthroughConnection> {
    pub fn new(
        info: ConnectionInfo,
        mirror_data_tx: broadcast::Sender<IncomingStreamItem>,
        request: ExtractedRequest<TokioIo<Box<dyn IncomingIO>>>,
    ) -> Self {
        let (request_frame_tx, request_frame_rx) = request
            .body_tail
            .is_some()
            .then(|| mpsc::channel::<Frame<Bytes>>(1))
            .unzip();

        let upgrade = tokio::spawn(async move {
            let version = request.parts.version;

            let body_tail = request_frame_rx
                .map(ReceiverStream::new)
                .map(|stream| stream.map(Ok))
                .map(StreamBody::new);
            let body = RolledBackBody {
                head: request.body_head.into_iter(),
                tail: body_tail,
            };
            let hyper_request = Request::from_parts(request.parts, body);

            let mut response = match Self::send_request(&info, hyper_request).await {
                Ok(response) => response,
                Err(error) => {
                    let message = format!(
                        "failed to pass the request to its original destination: {}",
                        Report::new(&error)
                    );
                    let error_response = MirrordErrorResponse::new(version, message);
                    let _ = request.response_tx.send(error_response.into());
                    return Err(error);
                }
            };

            let upgrade = (response.status() == StatusCode::SWITCHING_PROTOCOLS)
                .then(|| hyper::upgrade::on(&mut response));
            let _ = request.response_tx.send(response.map(BoxBody::new));

            match upgrade {
                Some(upgrade) => upgrade
                    .await
                    .map(Some)
                    .map_err(From::from)
                    .map_err(ConnError::PassthroughHttpError),
                None => Ok(None),
            }
        });

        let destination = PassthroughConnection {
            request_frame_tx,
            upgrade,
            mirror_data_tx,
        };

        Self {
            body_tail: request.body_tail,
            on_upgrade: request.upgrade,
            destination,
        }
    }

    async fn send_request<B>(
        info: &ConnectionInfo,
        request: Request<B>,
    ) -> Result<Response<Incoming>, ConnError>
    where
        B: 'static + Body<Data = Bytes, Error = hyper::Error> + Send + Unpin,
    {
        let stream = TcpStream::connect(info.pass_through_address())
            .await
            .map_err(From::from)
            .map_err(ConnError::TcpConnectError)?;

        let stream = match &info.tls_connector {
            Some(connector) => {
                let stream = connector
                    .connect(info.original_destination.ip(), Some(request.uri()), stream)
                    .await
                    .map_err(From::from)
                    .map_err(ConnError::TcpConnectError)?;
                MaybeTls::Tls(stream)
            }
            None => MaybeTls::NoTls(stream),
        };

        let version = match request.version() {
            Version::HTTP_2 => HttpVersion::V2,
            _ => HttpVersion::V1,
        };
        let mut sender = HttpSender::new(TokioIo::new(stream), version)
            .await
            .map_err(From::from)
            .map_err(ConnError::PassthroughHttpError)?;
        sender
            .send(request)
            .await
            .map_err(From::from)
            .map_err(ConnError::PassthroughHttpError)
    }
}

pub trait RequestDestination {
    type Upgraded: OutgoingDestination;

    fn send_frame(&mut self, frame: Frame<Bytes>) -> impl Future<Output = Result<(), ConnError>>;

    fn no_more_frames(&mut self) -> impl Future<Output = Result<(), ConnError>>;

    fn wait_for_upgrade(
        &mut self,
    ) -> impl Future<Output = Result<Option<Self::Upgraded>, ConnError>>;

    fn send_result(&mut self, result: Result<(), ConnError>) -> impl Future<Output = ()>;
}

pub struct StealingClient {
    pub data_tx: mpsc::Sender<IncomingStreamItem>,
    pub mirror_data_tx: broadcast::Sender<IncomingStreamItem>,
    pub upgrade_rx: oneshot::Receiver<Option<UpgradeDataRx>>,
}

impl RequestDestination for StealingClient {
    type Upgraded = StolenUpgrade;

    async fn send_frame(&mut self, frame: Frame<Bytes>) -> Result<(), ConnError> {
        let frame = frame
            .into_data()
            .map(Payload)
            .map(InternalHttpBodyFrame::Data)
            .unwrap_or_else(|frame| {
                let trailers = frame.into_trailers().expect("malformed frame");
                InternalHttpBodyFrame::Trailers(trailers)
            });
        let item = IncomingStreamItem::Frame(frame);
        let _ = self.mirror_data_tx.send(item.clone());
        self.data_tx
            .send(item)
            .await
            .map_err(|_| ConnError::StealerDropped)
    }

    async fn no_more_frames(&mut self) -> Result<(), ConnError> {
        let item = IncomingStreamItem::NoMoreFrames;
        let _ = self.mirror_data_tx.send(item.clone());
        self.data_tx
            .send(item)
            .await
            .map_err(|_| ConnError::StealerDropped)
    }

    async fn wait_for_upgrade(&mut self) -> Result<Option<Self::Upgraded>, ConnError> {
        let upgraded = (&mut self.upgrade_rx)
            .await
            .map_err(|_| ConnError::StealerDropped)?
            .map(|data_rx| StolenUpgrade {
                data_tx: self.data_tx.clone(),
                data_rx,
                mirror_data_tx: self.mirror_data_tx.clone(),
            });
        Ok(upgraded)
    }

    async fn send_result(&mut self, result: Result<(), ConnError>) {
        let item = IncomingStreamItem::Finished(result);
        let _ = self.mirror_data_tx.send(item.clone());
        let _ = self.data_tx.send(item).await;
    }
}

pub struct StolenUpgrade {
    data_tx: mpsc::Sender<IncomingStreamItem>,
    data_rx: mpsc::Receiver<Bytes>,
    mirror_data_tx: broadcast::Sender<IncomingStreamItem>,
}

impl OutgoingDestination for StolenUpgrade {
    async fn recv(&mut self) -> Result<CowBytes<'_>, ConnError> {
        Ok(CowBytes::Owned(
            self.data_rx.recv().await.unwrap_or_default(),
        ))
    }

    async fn send_data(&mut self, data: CowBytes<'_>) -> Result<(), ConnError> {
        let bytes = match data {
            CowBytes::Owned(bytes) => bytes,
            CowBytes::Borrowed(slice) => slice.to_vec().into(),
        };
        let item = IncomingStreamItem::Data(bytes);
        let _ = self.mirror_data_tx.send(item.clone());
        self.data_tx
            .send(item)
            .await
            .map_err(|_| ConnError::StealerDropped)
    }

    async fn shutdown(&mut self) -> Result<(), ConnError> {
        let item = IncomingStreamItem::NoMoreData;
        let _ = self.mirror_data_tx.send(item.clone());
        self.data_tx
            .send(item)
            .await
            .map_err(|_| ConnError::StealerDropped)
    }
}

pub struct PassthroughConnection {
    request_frame_tx: Option<mpsc::Sender<Frame<Bytes>>>,
    upgrade: JoinHandle<Result<Option<Upgraded>, ConnError>>,
    mirror_data_tx: broadcast::Sender<IncomingStreamItem>,
}

impl RequestDestination for PassthroughConnection {
    type Upgraded = UpgradedPassthroughConnection;

    async fn send_frame(&mut self, frame: Frame<Bytes>) -> Result<(), ConnError> {
        let item = frame
            .data_ref()
            .cloned()
            .map(Payload)
            .map(InternalHttpBodyFrame::Data)
            .or_else(|| {
                frame
                    .trailers_ref()
                    .cloned()
                    .map(InternalHttpBodyFrame::Trailers)
            })
            .map(IncomingStreamItem::Frame)
            .expect("malformed frame");
        let _ = self.mirror_data_tx.send(item);

        let Some(tx) = &self.request_frame_tx else {
            return Ok(());
        };
        if tx.send(frame).await.is_ok() {
            return Ok(());
        }

        let error = match (&mut self.upgrade).await {
            Err(..) => todo!("task panicked"),
            Ok(Err(error)) => error,
            Ok(Ok(..)) => todo!("unexpected finish"),
        };
        Err(error)
    }

    async fn no_more_frames(&mut self) -> Result<(), ConnError> {
        let _ = self.mirror_data_tx.send(IncomingStreamItem::NoMoreFrames);
        self.request_frame_tx = None;
        Ok(())
    }

    async fn send_result(&mut self, result: Result<(), ConnError>) {
        let _ = self
            .mirror_data_tx
            .send(IncomingStreamItem::Finished(result));
    }

    async fn wait_for_upgrade(&mut self) -> Result<Option<Self::Upgraded>, ConnError> {
        match (&mut self.upgrade).await {
            Err(..) => todo!("task panicked"),
            Ok(Err(error)) => Err(error),
            Ok(Ok(Some(upgraded))) => Ok(Some(UpgradedPassthroughConnection {
                upgraded: TokioIo::new(upgraded),
                buffer: BytesMut::with_capacity(64 * 1024),
                mirror_data_tx: self.mirror_data_tx.clone(),
            })),
            Ok(Ok(None)) => Ok(None),
        }
    }
}

pub struct UpgradedPassthroughConnection {
    upgraded: TokioIo<Upgraded>,
    buffer: BytesMut,
    mirror_data_tx: broadcast::Sender<IncomingStreamItem>,
}

impl OutgoingDestination for UpgradedPassthroughConnection {
    async fn recv(&mut self) -> Result<CowBytes<'_>, ConnError> {
        self.buffer.clear();
        self.upgraded
            .read(&mut self.buffer)
            .await
            .map_err(From::from)
            .map_err(ConnError::PassthroughIoError)?;
        Ok(CowBytes::Borrowed(&self.buffer))
    }

    async fn send_data(&mut self, data: CowBytes<'_>) -> Result<(), ConnError> {
        let bytes = match &data {
            CowBytes::Owned(bytes) => bytes.clone(),
            CowBytes::Borrowed(slice) => slice.to_vec().into(),
        };
        let _ = self.mirror_data_tx.send(IncomingStreamItem::Data(bytes));
        self.upgraded
            .write_all(data.as_ref())
            .await
            .map_err(From::from)
            .map_err(ConnError::PassthroughIoError)?;
        self.upgraded
            .flush()
            .await
            .map_err(From::from)
            .map_err(ConnError::PassthroughIoError)?;
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), ConnError> {
        let _ = self.mirror_data_tx.send(IncomingStreamItem::NoMoreData);
        self.upgraded
            .shutdown()
            .await
            .map_err(From::from)
            .map_err(ConnError::PassthroughIoError)?;
        Ok(())
    }
}
