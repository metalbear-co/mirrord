use std::{error::Report, future::Future, sync::Arc};

use bytes::{Bytes, BytesMut};
use futures::StreamExt;
use http_body_util::{BodyExt, StreamBody, combinators::BoxBody};
use hyper::{
    Request, Response,
    body::{Body, Frame, Incoming},
    http::{StatusCode, Version},
    upgrade::{OnUpgrade, Upgraded},
};
use hyper_util::rt::TokioIo;
use mirrord_protocol::{Payload, tcp::InternalHttpBodyFrame};
use mirrord_tls_util::MaybeTls;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use tokio_stream::wrappers::ReceiverStream;

use crate::{
    http::{
        HttpVersion, MIRRORD_AGENT_HTTP_HEADER_NAME, body::RolledBackBody,
        error::MirrordErrorResponse, extract_requests::ExtractedRequest, sender::HttpSender,
    },
    incoming::{
        IncomingStreamItem, RedirectorTaskConfig,
        connection::{
            ConnectionInfo,
            copy_bidirectional::{self, CowBytes, OutgoingDestination},
            optional_broadcast::OptionalBroadcast,
        },
        error::ConnError,
    },
};

pub type UpgradeDataRx = mpsc::Receiver<Bytes>;

/// Background task responsible for handling IO on a redirected HTTP request.
pub struct HttpTask<D> {
    /// Frames that we need to send to the request destination.
    pub body_tail: Option<Incoming>,
    /// Extracted from the original request.
    pub on_upgrade: OnUpgrade,
    /// Destination of the request.
    pub destination: D,
}

impl<D> HttpTask<D>
where
    D: RequestDestination,
{
    /// Runs this task until the request is finished.
    ///
    /// This method must ensure that [`Self::destination`] is notified about the result with
    /// [`RequestDestination::send_result`].
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

        let upgraded_peer = (&mut self.on_upgrade)
            .await
            .map_err(From::from)
            .map_err(ConnError::IncomingHttpError)?;
        let mut upgraded_peer = TokioIo::new(upgraded_peer);

        copy_bidirectional::copy_bidirectional(&mut upgraded_peer, &mut upgraded_destination).await
    }
}

impl HttpTask<PassthroughConnection> {
    pub fn new(
        info: ConnectionInfo,
        mirror_data_tx: OptionalBroadcast,
        request: ExtractedRequest,
        redirector_config: Arc<RedirectorTaskConfig>,
    ) -> Self {
        let (request_frame_tx, request_frame_rx) = request
            .body_tail
            .is_some()
            .then(|| mpsc::channel::<Frame<Bytes>>(1))
            .unzip();

        let redirector_config_clone = redirector_config.clone();
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
                        Report::new(&error).pretty(true)
                    );
                    let error_response = MirrordErrorResponse::new(version, message);
                    let _ = request.response_tx.send(error_response.into());
                    return Err(error);
                }
            };

            Self::modify_response(&mut response, &redirector_config_clone);

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

    /// Used for applying transformations on responses to
    /// passed-through requests.
    ///
    /// Currently just inserts the mirrord agent
    /// header.
    fn modify_response(
        response: &mut Response<Incoming>,
        redirector_config: &RedirectorTaskConfig,
    ) {
        if redirector_config.inject_headers {
            response.headers_mut().insert(
                MIRRORD_AGENT_HTTP_HEADER_NAME,
                http::HeaderValue::from_static("passed-through"),
            );
        }
    }
}

/// Destination of a redirected HTTP request, e.g. a stealing client or the original destination
/// HTTP server.
///
/// Implementors are allowed to panic if any of the methods is called after an error was returned.
pub trait RequestDestination {
    /// Type of the HTTP-upgraded connection.
    type Upgraded: OutgoingDestination;

    /// Sends the next HTTP body frame of the request.
    fn send_frame(&mut self, frame: Frame<Bytes>) -> impl Future<Output = Result<(), ConnError>>;

    /// Signals that there are no more HTTP body frames of the request.
    fn no_more_frames(&mut self) -> impl Future<Output = Result<(), ConnError>>;

    /// Waits for the HTTP exchange to finish and returns an optional HTTP-upgraded connection.
    ///
    /// Implementors are allowed to panic if this method is called more than once.
    fn wait_for_upgrade(
        &mut self,
    ) -> impl Future<Output = Result<Option<Self::Upgraded>, ConnError>>;

    /// Sends the result of the whole exchange.
    fn send_result(&mut self, result: Result<(), ConnError>) -> impl Future<Output = ()>;
}

/// Implementation of [`RequestDestination`] for a stealing client.
pub struct StealingClient {
    pub data_tx: mpsc::Sender<IncomingStreamItem>,
    pub mirror_data_tx: OptionalBroadcast,
    pub upgrade_rx: oneshot::Receiver<Option<UpgradeDataRx>>,
}

impl RequestDestination for StealingClient {
    type Upgraded = StolenUpgrade;

    async fn send_frame(&mut self, frame: Frame<Bytes>) -> Result<(), ConnError> {
        self.mirror_data_tx.send_frame(&frame);
        let frame = frame
            .into_data()
            .map(Payload)
            .map(InternalHttpBodyFrame::Data)
            .unwrap_or_else(|frame| {
                let trailers = frame.into_trailers().expect("malformed frame");
                InternalHttpBodyFrame::Trailers(trailers)
            });
        self.data_tx
            .send(IncomingStreamItem::Frame(frame))
            .await
            .map_err(|_| ConnError::StealerDropped)
    }

    async fn no_more_frames(&mut self) -> Result<(), ConnError> {
        self.mirror_data_tx
            .send_item(IncomingStreamItem::NoMoreFrames);
        self.data_tx
            .send(IncomingStreamItem::NoMoreFrames)
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
        self.mirror_data_tx.send_item(item.clone());
        let _ = self.data_tx.send(item).await;
    }
}

/// Implementation of [`OutgoingDestination`] for a stolen HTTP upgrade.
pub struct StolenUpgrade {
    data_tx: mpsc::Sender<IncomingStreamItem>,
    data_rx: mpsc::Receiver<Bytes>,
    mirror_data_tx: OptionalBroadcast,
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
        self.mirror_data_tx.send_item(item.clone());
        self.data_tx
            .send(item)
            .await
            .map_err(|_| ConnError::StealerDropped)
    }

    async fn shutdown(&mut self) -> Result<(), ConnError> {
        self.mirror_data_tx
            .send_item(IncomingStreamItem::NoMoreData);
        self.data_tx
            .send(IncomingStreamItem::NoMoreData)
            .await
            .map_err(|_| ConnError::StealerDropped)
    }
}

/// Implementation of [`RequestDestination`] for a passthrough connection to the original
/// destination.
pub struct PassthroughConnection {
    request_frame_tx: Option<mpsc::Sender<Frame<Bytes>>>,
    upgrade: JoinHandle<Result<Option<Upgraded>, ConnError>>,
    mirror_data_tx: OptionalBroadcast,
}

impl RequestDestination for PassthroughConnection {
    type Upgraded = UpgradedPassthroughConnection;

    async fn send_frame(&mut self, frame: Frame<Bytes>) -> Result<(), ConnError> {
        self.mirror_data_tx.send_frame(&frame);

        let Some(tx) = &self.request_frame_tx else {
            return Ok(());
        };
        if tx.send(frame).await.is_ok() {
            return Ok(());
        }

        let error = match (&mut self.upgrade).await {
            Err(error) => ConnError::AgentBug(format!(
                "passthrough request task failed: {error} [{}:{}]",
                file!(),
                line!()
            )),
            Ok(Err(error)) => error,
            Ok(Ok(..)) => ConnError::AgentBug(format!(
                "passthrough request task finished unexpectedly [{}:{}]",
                file!(),
                line!()
            )),
        };
        Err(error)
    }

    async fn no_more_frames(&mut self) -> Result<(), ConnError> {
        self.mirror_data_tx
            .send_item(IncomingStreamItem::NoMoreFrames);
        self.request_frame_tx = None;
        Ok(())
    }

    async fn send_result(&mut self, result: Result<(), ConnError>) {
        self.mirror_data_tx
            .send_item(IncomingStreamItem::Finished(result));
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

/// Implementation of [`OutgoingDestination`] for an upgraded HTTP passthrough connection.
pub struct UpgradedPassthroughConnection {
    upgraded: TokioIo<Upgraded>,
    buffer: BytesMut,
    mirror_data_tx: OptionalBroadcast,
}

impl OutgoingDestination for UpgradedPassthroughConnection {
    async fn recv(&mut self) -> Result<CowBytes<'_>, ConnError> {
        self.buffer.clear();
        self.upgraded
            .read_buf(&mut self.buffer)
            .await
            .map_err(From::from)
            .map_err(ConnError::PassthroughIoError)?;
        Ok(CowBytes::Borrowed(&self.buffer))
    }

    async fn send_data(&mut self, data: CowBytes<'_>) -> Result<(), ConnError> {
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
        self.mirror_data_tx.send_data(data);
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), ConnError> {
        self.mirror_data_tx
            .send_item(IncomingStreamItem::NoMoreData);
        self.upgraded
            .shutdown()
            .await
            .map_err(From::from)
            .map_err(ConnError::PassthroughIoError)?;
        Ok(())
    }
}
