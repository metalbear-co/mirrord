use std::fmt;

use hyper::http::{request, HeaderMap, Method, Uri, Version};
use hyper_util::rt::TokioIo;
use mirrord_protocol::tcp::InternalHttpBodyFrame;
use tokio::{
    runtime::Handle,
    sync::{mpsc, oneshot},
};

use super::{
    http_passthrough::PassThroughTask,
    http_steal::{StealTask, UpgradeDataRx},
    ConnectionInfo, IncomingIO, IncomingStream,
};
use crate::http::extract_requests::{BoxResponse, ExtractedRequest};

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
