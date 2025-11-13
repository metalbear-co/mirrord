use core::str::FromStr;
use std::{
    fmt::{self, Debug},
    ops::Not,
};

use bytes::Bytes;
use futures::StreamExt;
use http::{header::CONTENT_LENGTH, request::Parts};
use http_body_util::{BodyExt, StreamBody, combinators::BoxBody};
use hyper::{
    Response,
    body::Frame,
    http::{HeaderMap, Method, StatusCode, Uri, Version, request, response},
};
use mirrord_protocol::tcp::InternalHttpBodyFrame;
use tokio::{
    runtime::Handle,
    sync::{broadcast, mpsc, oneshot},
    time::Instant,
};
use tokio_stream::wrappers::{BroadcastStream, ReceiverStream};
use tracing::instrument;

use super::{ConnectionInfo, IncomingStream};
use crate::{
    http::{BoxResponse, body::RolledBackBody, extract_requests::ExtractedRequest},
    incoming::{
        IncomingStreamItem, RedirectorTaskConfig,
        connection::http_task::{HttpTask, StealingClient, UpgradeDataRx},
    },
    util::body_buffering::{
        BufferBodyError, BufferedBody, FramesReader, MAX_BODY_BUFFER_SIZE, MAX_BODY_BUFFER_TIMEOUT,
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

    buffered_body: BufferedBody<Frame<Bytes>>,
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
            buffered_body: BufferedBody::Empty,
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
                uri: self.request.parts.uri.clone(),
                method: self.request.parts.method.clone(),
                headers: self.request.parts.headers.clone(),
                version: self.request.parts.version,
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
                body_finished: self.request.body_tail.is_none() && self.buffered_body.is_empty(),
            },
            parts: self.request.parts.clone(),
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
            uri: self.request.parts.uri,
            method: self.request.parts.method,
            headers: self.request.parts.headers,
            version: self.request.parts.version,
            body_head: self
                .request
                .body_head
                .into_iter()
                .map(InternalHttpBodyFrame::from)
                .collect(),
            body_finished: self.request.body_tail.is_none() && self.buffered_body.is_empty(),
        };

        let task = HttpTask {
            body_tail: self.request.body_tail,
            on_upgrade: self.request.upgrade,
            destination: StealingClient {
                data_tx: tx,
                mirror_data_tx: self.mirror_tx.into(),
                upgrade_rx,
            },
            buffered_body: self.buffered_body,
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
            self.buffered_body,
        );
        self.runtime_handle.spawn(task.run());
    }

    #[instrument(level = "trace", ret)]
    pub async fn buffer_body(&mut self) -> Result<(), BufferBodyError> {
        if self.buffered_body.is_empty().not() {
            tracing::error!(
                buffered_body = ?self.buffered_body,
                "buffer_body called more than once. This is a bug, please report."
            );
            return Ok(());
        }

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

        let mut buffered = Vec::new();
        let mut received_data_bytes = 0;

        // We are forced to drain all of `body_head` regardless of its
        // size because it will be sent *before* the buffered body so
        // if we leave any trailing frames the order will get messed
        // up :(
        // In any case the body head is unlikely to be longer than `MAX_BODY_SIZE`
        // so it shouldn't matter :D

        let mut got_trailers = false;
        for frame in self.request.body_head.drain(..) {
            match frame.data_ref() {
                Some(data) => received_data_bytes += data.len(),
                None => got_trailers = true,
            }
            buffered.push(frame);
        }

        if got_trailers {
            return Ok(());
        }

        let Some(tail) = self.request.body_tail.as_mut() else {
            tracing::debug!("request has no tail, bailing early");
            self.buffered_body = BufferedBody::Successful(buffered);
            return Ok(());
        };

        let until = Instant::now() + *MAX_BODY_BUFFER_TIMEOUT;

        let result = loop {
            let frame = tokio::time::timeout_at(until, tail.frame()).await;

            let frame = match frame {
                Ok(Some(Ok(f))) => f,
                Ok(None) => break Ok(()),
                Err(elapsed) => break Err(elapsed.into()),
                Ok(Some(Err(err))) => break Err(err.into()),
            };

            buffered.push(frame);

            let Some(data) = buffered.last().unwrap().data_ref() else {
                break Ok(());
            };

            received_data_bytes += data.len();

            if received_data_bytes > *MAX_BODY_BUFFER_SIZE {
                break Err(BufferBodyError::BodyTooBig);
            }
        };

        match &result {
            Ok(()) => {
                self.buffered_body = BufferedBody::Successful(buffered);
            }
            Err(_) => {
                self.buffered_body = BufferedBody::Failed(buffered);
            }
        };

        result
    }

    pub fn parts_and_body(&mut self) -> (&mut Parts, Option<FramesReader<'_, Frame<Bytes>>>) {
        (&mut self.request.parts, self.buffered_body.reader())
    }
}

impl Debug for RedirectedHttp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RedirectedHttp")
            .field("info", &self.info)
            .field("request", &self.request)
            .field("buffered_body", &self.buffered_body)
            .finish()
    }
}

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
    pub uri: Uri,
    pub method: Method,
    pub headers: HeaderMap,
    pub version: Version,
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
    /// The original request parts from ExtractedRequest, used for HTTP filtering
    pub parts: request::Parts,
    /// Will not return frames that are already in [`Self::request_head`].
    pub stream: IncomingStream,
}

impl MirroredHttp {
    /// Returns a mutable reference to the request parts and a shared
    /// reference to buffered body, if any.
    pub fn parts(&mut self) -> &mut request::Parts {
        &mut self.parts
    }
}

impl Debug for MirroredHttp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MirroredHttp")
            .field("info", &self.info)
            .field("request_head", &self.request_head)
            .field("parts", &self.parts)
            .finish()
    }
}
