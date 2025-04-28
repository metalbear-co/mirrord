use bytes::Bytes;
use hyper::{
    http::{request, HeaderMap, Method, Uri, Version},
    Response,
};
use mirrord_protocol::tcp::InternalHttpBodyFrame;
use service::ExtractedRequest;
use steal::UpgradeDataRx;
use tokio::{
    runtime::Handle,
    sync::{broadcast, mpsc, oneshot},
};
use tokio_stream::wrappers::BroadcastStream;

use super::{ConnectionInfo, IncomingStream, IncomingStreamItem};

pub mod error_response;
mod passthrough;
pub mod service;
mod steal;

/// [`Body`](hyper::body::Body) type used by the request task.
pub type BoxBody = http_body_util::combinators::BoxBody<Bytes, hyper::Error>;
/// [`Response`] type used by the request task.
pub type BoxResponse = Response<BoxBody>;

/// A redirected HTTP request.
///
/// No data is received nor sent via for this request until the connection task
/// is started with either [`Self::steal`] or [`Self::pass_through`].
pub struct RedirectedHttp {
    request: ExtractedRequest,
    info: ConnectionInfo,
    /// [`None`] until [`Self::mirror`] is called.
    copy_tx: Option<broadcast::Sender<IncomingStreamItem>>,
    /// Handle to the [`tokio::runtime`] in which this struct was created.
    ///
    /// Used to spawn the connection task.
    ///
    /// Thanks to this handle, this struct can be freely moved across runtimes.
    runtime_handle: Handle,
}

impl RedirectedHttp {
    /// Should be called in the target's Linux network namespace.
    pub fn new(info: ConnectionInfo, request: ExtractedRequest) -> Self {
        Self {
            request,
            info,
            copy_tx: None,
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
    pub fn mirror(&mut self) -> MirroredHttp {
        let rx = self
            .copy_tx
            .as_ref()
            .map(|tx| tx.subscribe())
            .unwrap_or_else(|| {
                let (tx, rx) = broadcast::channel(32);
                self.copy_tx.replace(tx);
                rx
            });

        MirroredHttp {
            info: self.info.clone(),
            request_head: RequestHead {
                uri: self.request.parts.uri.clone(),
                method: self.request.parts.method.clone(),
                headers: self.request.parts.headers.clone(),
                version: self.request.parts.version,
                body: self
                    .request
                    .body_head
                    .iter()
                    .map(InternalHttpBodyFrame::from)
                    .collect(),
                has_more_frames: self.request.body_tail.is_some(),
            },
            stream: IncomingStream::Broadcast(BroadcastStream::new(rx)),
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
            body: self
                .request
                .body_head
                .into_iter()
                .map(InternalHttpBodyFrame::from)
                .collect(),
            has_more_frames: self.request.body_tail.is_some(),
        };

        let task = steal::StealTask {
            body_tail: self.request.body_tail,
            on_upgrade: self.request.on_upgrade,
            upgrade_rx,
            tx: tx.into(),
            copy_tx: self.copy_tx.into(),
        };
        self.runtime_handle.spawn(task.run());

        StolenHttp {
            info: self.info,
            request_head,
            stream: IncomingStream::Mpsc(rx),
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
        let task = passthrough::PassThroughTask {
            info: self.info,
            copy_tx: self.copy_tx.into(),
        };
        self.runtime_handle.spawn(task.run(self.request));
    }
}

/// Mirror handle to a redirected HTTP request.
pub struct MirroredHttp {
    pub info: ConnectionInfo,
    pub request_head: RequestHead,
    /// Will not return frames that are already in [`Self::request_head`].
    pub stream: IncomingStream,
}

/// Steal handle to a redirected HTTP request.
pub struct StolenHttp {
    pub info: ConnectionInfo,
    pub request_head: RequestHead,
    /// Will not return frames that are already in [`Self::request_head`].
    pub stream: IncomingStream,
    pub response_provider: ResponseProvider,
}

/// Head of a redirected HTTP request.
pub struct RequestHead {
    pub uri: Uri,
    pub method: Method,
    pub headers: HeaderMap,
    pub version: Version,
    pub body: Vec<InternalHttpBodyFrame>,
    pub has_more_frames: bool,
}

/// Can be used to by a stealing client to send an HTTP response for a stolen HTTP request.
pub struct ResponseProvider {
    response_tx: oneshot::Sender<BoxResponse>,
    upgrade_tx: oneshot::Sender<Option<UpgradeDataRx>>,
}

impl ResponseProvider {
    /// Sends the response, notifying the request task that there is no HTTP upgrade.
    pub fn send(self, response: BoxResponse) {
        let _ = self.response_tx.send(response);
        let _ = self.upgrade_tx.send(None);
    }

    /// Sends the response, notifying the request task that it should expect an HTTP upgrade.
    ///
    /// Returns an [`mpsc::Sender`] that can be used to send raw data to the peer.
    /// Dropping this sender will be interpreted as a write shutdown,
    /// and will not terminate the connection right away.
    pub fn send_with_upgrade(self, response: BoxResponse) -> mpsc::Sender<Vec<u8>> {
        let (data_tx, data_rx) = mpsc::channel(8);

        let _ = self.response_tx.send(response);
        let _ = self.upgrade_tx.send(Some(data_rx));

        data_tx
    }
}
