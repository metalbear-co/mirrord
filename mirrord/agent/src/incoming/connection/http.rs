use std::fmt;

use bytes::Bytes;
use futures::StreamExt;
use http_body_util::{combinators::BoxBody, StreamBody};
use hyper::{
    body::Frame,
    http::{request, response, HeaderMap, Method, StatusCode, Uri, Version},
    Response,
};
use hyper_util::rt::TokioIo;
use mirrord_protocol::tcp::InternalHttpBodyFrame;
use tokio::{
    runtime::Handle,
    sync::{mpsc, oneshot},
};
use tokio_stream::wrappers::ReceiverStream;

use super::{
    http_passthrough::PassThroughTask,
    http_steal::{StealTask, UpgradeDataRx},
    ConnectionInfo, IncomingIO, IncomingStream,
};
use crate::http::{body::RolledBackBody, extract_requests::ExtractedRequest, BoxResponse};

/// A redirected HTTP request.
///
/// No data is received nor sent via for this request until the connection task
/// is started with either [`Self::steal`] or [`Self::pass_through`].
pub struct RedirectedHttp {
    request: ExtractedRequest<TokioIo<Box<dyn IncomingIO>>>,
    info: ConnectionInfo,
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
    pub fn new(
        info: ConnectionInfo,
        request: ExtractedRequest<TokioIo<Box<dyn IncomingIO>>>,
    ) -> Self {
        Self {
            request,
            info,
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

        let task = StealTask {
            body_tail: self.request.body_tail,
            on_upgrade: self.request.upgrade,
            upgrade_rx,
            tx: tx.into(),
        };
        self.runtime_handle.spawn(task.run());

        StolenHttp {
            info: self.info,
            request_head,
            stream: IncomingStream { rx: Some(rx) },
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
        let task = PassThroughTask { info: self.info };
        self.runtime_handle.spawn(task.run(self.request));
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
    pub fn finish(self) -> Option<mpsc::Sender<Vec<u8>>> {
        let (data_tx, data_rx) = self.has_upgrade.then(|| mpsc::channel(8)).unzip();
        let _ = self.upgrade_tx.send(data_rx);
        data_tx
    }
}
