use std::{
    fmt::{self, Debug},
    str::FromStr,
    sync::{Arc, LazyLock},
    time::Duration,
};

use bytes::Bytes;
use futures::StreamExt;
use http::{header::CONTENT_LENGTH, request::Parts};
use http_body_util::{BodyExt, StreamBody, combinators::BoxBody};
use hyper::{
    Response,
    body::Frame,
    http::{StatusCode, request, response},
};
use mirrord_agent_env::envs;
use mirrord_protocol::tcp::InternalHttpBodyFrame;
use tokio::{
    runtime::Handle,
    sync::{broadcast, mpsc, oneshot},
    time::error::Elapsed,
};
use tokio_stream::wrappers::{BroadcastStream, ReceiverStream};
use tracing::instrument;

use super::{ConnectionInfo, IncomingStream, body_utils::FramesReader};
use crate::{
    http::{BoxResponse, body::RolledBackBody, extract_requests::ExtractedRequest},
    incoming::{
        ConnError, IncomingStreamItem, RedirectorTaskConfig,
        connection::{
            http_task::{HttpTask, StealingClient, UpgradeDataRx},
            optional_broadcast::OptionalBroadcast,
        },
    },
};

/// A redirected HTTP request.
///
/// No data is received nor sent via for this request until the connection task
/// is started with either [`Self::steal`] or [`Self::pass_through`].
pub struct RedirectedHttp {
    request: ExtractedRequest,
    info: ConnectionInfo,
    mirror_tx: Option<broadcast::Sender<IncomingStreamItem>>,
    /// Handle to the [`tokio::runtime`] in which this struct was created.
    ///
    /// Used to spawn the connection task.
    ///
    /// Thanks to this handle, this struct can be freely moved across runtimes.
    runtime_handle: Handle,

    /// Configuration of the RedirectorTask that created this
    redirector_config: RedirectorTaskConfig,
}

#[derive(thiserror::Error, Debug)]
pub enum BufferBodyError {
    #[error(transparent)]
    Conn(#[from] ConnError),
    #[error("body size exceeded max configured size of {} bytes", *MAX_BODY_BUFFER_SIZE)]
    BodyTooBig,
    #[error("receiving body took longer than the max configured timeout of {}ms", MAX_BODY_BUFFER_TIMEOUT.as_millis())]
    Timeout(#[from] Elapsed),
}

impl RedirectedHttp {
    /// Should be called in the target's Linux network namespace,
    /// as [`Handle::current()`] is stored in this struct.
    /// We might need to connect to the original destination in the future.
    pub fn new(
        info: ConnectionInfo,
        request: ExtractedRequest,
        redirector_config: RedirectorTaskConfig,
    ) -> Self {
        Self {
            request,
            info,
            mirror_tx: None,
            runtime_handle: Handle::current(),
            redirector_config,
        }
    }

    pub fn info(&self) -> &ConnectionInfo {
        &self.info
    }

    pub fn parts(&self) -> &request::Parts {
        &self.request.parts
    }

    /// Acquires a mirror handle to this request.
    ///
    /// For the data to flow, you must start the request task with either [`Self::steal`] or
    /// [`Self::pass_through`].
    pub fn mirror(&mut self) -> MirroredHttp {
        let rx = match &self.mirror_tx {
            Some(tx) => tx.subscribe(),
            None => {
                let (tx, rx) = broadcast::channel(32);
                self.mirror_tx = Some(tx);
                rx
            }
        };

        MirroredHttp {
            info: self.info.clone(),
            request_head: RequestHead {
                parts: self.request.parts.clone(),
                body_head: self
                    .request
                    .body_head
                    .iter()
                    .map(|frame| {
                        frame
                            .data_ref()
                            .cloned()
                            .map(From::from)
                            .map(InternalHttpBodyFrame::Data)
                            .or_else(|| {
                                frame
                                    .trailers_ref()
                                    .cloned()
                                    .map(InternalHttpBodyFrame::Trailers)
                            })
                            .expect("malformed frame")
                    })
                    .collect(),
                body_finished: self.request.body_tail.is_none(),
            },
            stream: IncomingStream::Mirror(BroadcastStream::new(rx)),
        }
    }

    /// Acquires a steal handle to this request,
    /// and starts the request task in the background.
    ///
    /// All data will be directed to this handle.
    pub fn steal(self) -> StolenHttp {
        let (tx, rx) = mpsc::channel(8);
        let (upgrade_tx, upgrade_rx) = oneshot::channel();

        let request_head = RequestHead {
            parts: self.request.parts.clone(),
            body_head: self
                .request
                .body_head
                .into_iter()
                .map(InternalHttpBodyFrame::from)
                .collect(),
            body_finished: self.request.body_tail.is_none(),
        };

        let task = HttpTask {
            body_tail: self.request.body_tail,
            on_upgrade: self.request.upgrade,
            destination: StealingClient {
                data_tx: tx,
                mirror_data_tx: self.mirror_tx.into(),
                upgrade_rx,
            },
        };
        self.runtime_handle.spawn(task.run());

        StolenHttp {
            info: self.info,
            request_head,
            stream: IncomingStream::Steal(rx),
            response_provider: ResponseProvider {
                response_tx: self.request.response_tx,
                upgrade_tx,
            },
            redirector_config: self.redirector_config,
        }
    }

    /// Starts the request task in the background.
    ///
    /// All data will be directed to the original destination.
    pub fn pass_through(self) {
        let task = HttpTask::new(
            self.info,
            self.mirror_tx.into(),
            self.request,
            self.redirector_config,
        );
        self.runtime_handle.spawn(task.run());
    }

    pub fn parts_and_body(&mut self) -> (&mut Parts, Option<FramesReader<'_, Frame<Bytes>>>) {
        (
            &mut self.request.parts,
            self.request
                .body_tail
                .is_none()
                .then_some(FramesReader::from(&*self.request.body_head)),
        )
    }

    #[instrument(level = "trace", ret)]
    pub async fn buffer_body(&mut self) -> Result<(), BufferBodyError> {
        let Some(tail) = self.request.body_tail.as_mut() else {
            return Ok(());
        };

        let content_length = self
            .request
            .parts
            .headers
            .get(CONTENT_LENGTH)
            .and_then(|t| t.to_str().ok())
            .and_then(|t| usize::from_str(t).ok());

        if content_length.is_some_and(|l| l > *MAX_BODY_BUFFER_SIZE) {
            return Err(BufferBodyError::BodyTooBig);
        }

        let mut rxd: usize = self
            .request
            .body_head
            .iter()
            .map(|f| f.data_ref().map(Bytes::len).unwrap_or(0))
            .sum();

        let result = tokio::time::timeout(*MAX_BODY_BUFFER_TIMEOUT, async {
            let mut mirror = OptionalBroadcast::from(self.mirror_tx.clone());
            while rxd < *MAX_BODY_BUFFER_SIZE {
                match tail.frame().await {
                    None => {
                        mirror.send_item(IncomingStreamItem::NoMoreFrames);
                        return Ok(());
                    }
                    Some(Ok(frame)) => {
                        rxd += frame.data_ref().map(|f| f.len()).unwrap_or(0);
                        mirror.send_frame(&frame);
                        self.request.body_head.push(frame);
                    }
                    Some(Err(error)) => {
                        let error = ConnError::IncomingHttpError(Arc::new(error));
                        mirror.send_item(IncomingStreamItem::Finished(Err(error.clone())));
                        Err(error)?
                    }
                }
            }
            Err(BufferBodyError::BodyTooBig)
        })
        .await?;

        // Set body_tail to none since we've extracted everything from it
        result.inspect(|_| self.request.body_tail = None)
    }
}

impl Debug for RedirectedHttp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RedirectedHttp")
            .field("info", &self.info)
            .field("request", &self.request)
            .finish()
    }
}

static MAX_BODY_BUFFER_SIZE: LazyLock<usize> = LazyLock::new(|| {
    match envs::MAX_BODY_BUFFER_SIZE.try_from_env() {
        Ok(Some(t)) => Some(t as usize),
        Ok(None) => {
            tracing::warn!("{} not set, using default", envs::MAX_BODY_BUFFER_SIZE.name);
            None
        }
        Err(error) => {
            tracing::warn!(
                ?error,
                "failed to parse {}, using default",
                envs::MAX_BODY_BUFFER_SIZE.name
            );
            None
        }
    }
    .unwrap_or(64 * 1024)
});

static MAX_BODY_BUFFER_TIMEOUT: LazyLock<Duration> = LazyLock::new(|| {
    Duration::from_millis(
        match envs::MAX_BODY_BUFFER_TIMEOUT.try_from_env() {
            Ok(Some(t)) => Some(t),
            Ok(None) => {
                tracing::warn!(
                    "{} not set, using default",
                    envs::MAX_BODY_BUFFER_TIMEOUT.name
                );
                None
            }
            Err(error) => {
                tracing::warn!(
                    ?error,
                    "failed to parse {}, using default",
                    envs::MAX_BODY_BUFFER_TIMEOUT.name
                );
                None
            }
        }
        .unwrap_or(1000)
        .into(),
    )
});
/// Steal handle to a redirected HTTP request.
pub struct StolenHttp {
    pub info: ConnectionInfo,
    pub request_head: RequestHead,
    /// Will not return frames that are already in [`Self::request_head`].
    pub stream: IncomingStream,
    pub response_provider: ResponseProvider,
    pub redirector_config: RedirectorTaskConfig,
}

impl Debug for StolenHttp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StolenHttp")
            .field("info", &self.info)
            .field("request_head", &self.request_head)
            .finish()
    }
}

/// Head of a redirected HTTP request.
#[derive(Debug)]
pub struct RequestHead {
    pub parts: Parts,
    pub body_head: Vec<InternalHttpBodyFrame>,
    pub body_finished: bool,
}

/// Can be used by a stealing client to send an HTTP response for a stolen HTTP request.
pub struct ResponseProvider {
    response_tx: oneshot::Sender<BoxResponse>,
    upgrade_tx: oneshot::Sender<Option<UpgradeDataRx>>,
}

impl ResponseProvider {
    /// Starts the response to the original HTTP client.
    ///
    /// Use this method only when you don't have the full body.
    ///
    /// Returns a [`ResponseBodyProvider`].
    pub fn send(self, parts: response::Parts) -> ResponseBodyProvider {
        let has_upgrade = parts.status == StatusCode::SWITCHING_PROTOCOLS;
        let (frame_tx, frame_rx) = mpsc::channel::<Frame<Bytes>>(8);
        let body = RolledBackBody {
            head: Default::default(),
            tail: Some(StreamBody::new(ReceiverStream::new(frame_rx).map(Ok))),
        };

        let response = Response::from_parts(parts, BoxBody::new(body));
        let _ = self.response_tx.send(response);

        ResponseBodyProvider {
            has_upgrade,
            upgrade_tx: self.upgrade_tx,
            frame_tx,
        }
    }

    /// Sends the full response to the original HTTP client.
    ///
    /// Use this method *always* when you have the full body.
    ///
    /// Returns an optional channel to send data after an HTTP upgrade.
    /// Dropping this channel will be interpreted as a write shutdown.
    ///
    /// # Rationale
    ///
    /// Sending all body immediately matters when handling gRPC error responses.
    /// If we don't make all frames instantly available, hyper will not set END_STREAM flag on the
    /// headers frame, and gRPC client will fail with something like "server closed connection with
    /// RST_STREAM without sending trailers".
    pub fn send_finished(
        self,
        response: Response<BoxBody<Bytes, hyper::Error>>,
    ) -> Option<mpsc::Sender<Bytes>> {
        let has_upgrade = response.status() == StatusCode::SWITCHING_PROTOCOLS;
        let _ = self.response_tx.send(response);
        let (data_tx, data_rx) = has_upgrade.then(|| mpsc::channel(8)).unzip();
        let _ = self.upgrade_tx.send(data_rx);
        data_tx
    }
}

/// Can be used by a stealing client to send HTTP response body frames.
pub struct ResponseBodyProvider {
    has_upgrade: bool,
    upgrade_tx: oneshot::Sender<Option<UpgradeDataRx>>,
    frame_tx: mpsc::Sender<Frame<Bytes>>,
}

impl ResponseBodyProvider {
    pub async fn send_frame(&self, frame: Frame<Bytes>) {
        let _ = self.frame_tx.send(frame).await;
    }

    /// Signals that the response body is finished.
    ///
    /// Returns an optional channel to send data after an HTTP upgrade.
    /// Dropping this channel will be interpreted as a write shutdown.
    pub fn finish(self) -> Option<mpsc::Sender<Bytes>> {
        let (data_tx, data_rx) = self.has_upgrade.then(|| mpsc::channel(8)).unzip();
        let _ = self.upgrade_tx.send(data_rx);
        data_tx
    }
}

/// Mirror handle to a redirected HTTP request.
pub struct MirroredHttp {
    pub info: ConnectionInfo,
    pub request_head: RequestHead,
    /// Will not return frames that are already in [`Self::request_head`].
    pub stream: IncomingStream,
}

impl MirroredHttp {
    /// Returns a mutable reference to the request parts and a shared
    /// reference to buffered body, if any.
    pub fn parts_and_body(
        &mut self,
    ) -> (
        &mut request::Parts,
        Option<FramesReader<'_, InternalHttpBodyFrame>>,
    ) {
        (
            &mut self.request_head.parts,
            self.request_head
                .body_finished
                .then_some(FramesReader::from(&*self.request_head.body_head)),
        )
    }

    #[instrument(level = "trace", ret)]
    pub async fn buffer_body(&mut self) -> Result<(), BufferBodyError> {
        if self.request_head.body_finished {
            return Ok(());
        }

        let content_length = self
            .request_head
            .parts
            .headers
            .get(CONTENT_LENGTH)
            .and_then(|t| t.to_str().ok())
            .and_then(|t| usize::from_str(t).ok());

        if content_length.is_some_and(|l| l > *MAX_BODY_BUFFER_SIZE) {
            return Err(BufferBodyError::BodyTooBig);
        }

        let mut rxd: usize = self
            .request_head
            .body_head
            .iter()
            .map(|f| match f {
                InternalHttpBodyFrame::Data(p) => p.len(),
                InternalHttpBodyFrame::Trailers(_) => 0,
            })
            .sum();

        let result = tokio::time::timeout(*MAX_BODY_BUFFER_TIMEOUT, async {
            while rxd < *MAX_BODY_BUFFER_SIZE {
                match self.stream.next().await {
                    Some(IncomingStreamItem::Frame(f)) => {
                        if let InternalHttpBodyFrame::Data(data) = &f {
                            rxd += data.len();
                        }
                        self.request_head.body_head.push(f);
                    }
                    Some(IncomingStreamItem::NoMoreFrames) => return Ok(()),
                    Some(IncomingStreamItem::Finished(error @ Err(_))) => error?,
                    other =>
                        Err(ConnError::AgentBug(format!(
                            "received an unexpected IncomingStreamItem when buffering HTTP request body: {other:?}"
                        )))?
                }
            }
            Err(BufferBodyError::BodyTooBig)
        })
        .await?;

        // Set body_tail to none since we've extracted everything from it
        result.inspect(|_| self.request_head.body_finished = true)
    }
}

impl Debug for MirroredHttp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MirroredHttp")
            .field("info", &self.info)
            .field("request_head", &self.request_head)
            .finish()
    }
}
