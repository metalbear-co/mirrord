use std::fmt;

use bytes::Bytes;
use futures::StreamExt;
use http_body_util::{combinators::BoxBody, StreamBody};
use hyper::{
    body::Frame,
    http::{request, response, HeaderMap, Method, StatusCode, Uri, Version},
    Response,
};
use mirrord_protocol::tcp::InternalHttpBodyFrame;
use tokio::{
    runtime::Handle,
    sync::{broadcast, mpsc, oneshot},
};
use tokio_stream::wrappers::{BroadcastStream, ReceiverStream};

use super::{ConnectionInfo, IncomingStream};
use crate::{
    http::{body::RolledBackBody, extract_requests::ExtractedRequest, BoxResponse},
    incoming::{
        connection::http_task::{HttpTask, StealingClient, UpgradeDataRx},
        IncomingStreamItem,
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
}

impl RedirectedHttp {
    /// Should be called in the target's Linux network namespace,
    /// as [`Handle::current()`] is stored in this struct.
    /// We might need to connect to the original destination in the future.
    pub fn new(info: ConnectionInfo, request: ExtractedRequest) -> Self {
        Self {
            request,
            info,
            mirror_tx: None,
            runtime_handle: Handle::current(),
        }
    }

    pub fn info(&self) -> &ConnectionInfo {
        &self.info
    }

    pub fn parts(&self) -> &request::Parts {
        &self.request.parts
    }

    pub fn parts_mut(&mut self) -> &mut request::Parts {
        &mut self.request.parts
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
        }
    }

    /// Starts the request task in the background.
    ///
    /// All data will be directed to the original destination.
    pub fn pass_through(self) {
        let task = HttpTask::new(self.info, self.mirror_tx.into(), self.request);
        self.runtime_handle.spawn(task.run());
    }
}

impl fmt::Debug for RedirectedHttp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RedirectedHttp")
            .field("info", &self.info)
            .field("request", &self.request)
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
}

impl fmt::Debug for StolenHttp {
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
    /// Sends the response to the original HTTP client.
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

impl fmt::Debug for MirroredHttp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MirroredHttp")
            .field("info", &self.info)
            .field("request_head", &self.request_head)
            .finish()
    }
}
