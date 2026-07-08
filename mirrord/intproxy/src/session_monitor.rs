use hyper::{HeaderMap, Uri};
use mirrord_protocol::tcp::InternalHttpBodyFrame;
use serde::Serialize;
use tokio::sync::broadcast;

pub mod api;
pub mod chaos;

/// Wrapper around `Vec<String>` that redacts its [`Debug`] output to avoid leaking environment
/// variable names into logs, while still serializing normally for the session monitor API.
#[derive(Clone, Serialize)]
#[serde(transparent)]
pub struct RedactedVarNames(pub Vec<String>);

impl core::fmt::Debug for RedactedVarNames {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_tuple("RedactedVarNames")
            .field(&"<REDACTED>")
            .finish()
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MonitorEvent {
    FileOp {
        path: Option<String>,
        operation: String,
    },
    DnsQuery {
        host: String,
    },
    IncomingRequest {
        id: String,
        method: String,
        path: String,
        host: String,
        port: u16,
        http_version: String,
        headers: Vec<(String, String)>,
    },
    IncomingResponse {
        id: String,
        status: u16,
        method: String,
        path: String,
        http_version: String,
        headers: Vec<(String, String)>,
    },
    IncomingRequestBody {
        id: String,
        method: String,
        path: String,
        body: String,
        truncated: bool,
        bytes: usize,
    },
    IncomingResponseBody {
        id: String,
        status: u16,
        method: String,
        path: String,
        body: String,
        truncated: bool,
        bytes: usize,
    },
    OutgoingConnection {
        address: String,
        port: u16,
    },
    PortSubscription {
        port: u16,
        mode: String,
    },
    EnvVar {
        vars: RedactedVarNames,
    },
    LayerConnected {
        pid: u32,
        parent_pid: Option<u32>,
        process_name: String,
        cmdline: Vec<String>,
    },
    LayerDisconnected {
        pid: u32,
    },
}

/// Default byte limit for HTTP body previews attached to session monitor events, used when the
/// user hasn't overridden `experimental.session_monitor_body_limit`. Bodies are captured up to
/// this limit and marked as truncated beyond it, keeping event fan-out memory bounded.
pub const BODY_CAPTURE_LIMIT: usize = 32 * 1024;

/// Accumulates an HTTP body for session monitor events, keeping at most `limit` bytes while
/// counting the total size seen.
#[derive(Debug)]
pub struct BodyCapture {
    buf: Vec<u8>,
    truncated: bool,
    bytes: usize,
    limit: usize,
}

impl BodyCapture {
    pub fn new(limit: usize) -> Self {
        Self {
            buf: Vec::new(),
            truncated: false,
            bytes: 0,
            limit,
        }
    }

    pub fn extend(&mut self, data: &[u8]) {
        self.bytes += data.len();
        let remaining = self.limit.saturating_sub(self.buf.len());
        let take = remaining.min(data.len());
        if let Some(slice) = data.get(..take) {
            self.buf.extend_from_slice(slice);
        }
        if take < data.len() {
            self.truncated = true;
        }
    }

    pub fn extend_frames<'a, I>(&mut self, frames: I)
    where
        I: IntoIterator<Item = &'a InternalHttpBodyFrame>,
    {
        for frame in frames {
            if let InternalHttpBodyFrame::Data(data) = frame {
                self.extend(&data.0);
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.bytes == 0
    }

    /// Returns the captured preview (lossy UTF-8), whether it was truncated, and the total
    /// number of body bytes seen.
    pub fn finish(self) -> (String, bool, usize) {
        (
            String::from_utf8_lossy(&self.buf).into_owned(),
            self.truncated,
            self.bytes,
        )
    }
}

/// Returns the request path including the query string for session monitor events. The query
/// must be preserved because the `mirrord ui` HAR export reconstructs the full request URL
/// from this field.
pub fn uri_path_and_query(uri: &Uri) -> String {
    uri.path_and_query()
        .map(|pq| pq.as_str().to_owned())
        .unwrap_or_else(|| uri.path().to_owned())
}

/// Flattens a [`HeaderMap`] into name/value pairs for session monitor events, preserving
/// repeated header names and replacing non-UTF-8 bytes in values.
pub fn header_pairs(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_owned(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            )
        })
        .collect()
}

/// Wrapper around an optional broadcast sender for session monitor events.
///
/// When the session monitor is disabled, this wraps `None` and all emit calls are no-ops.
#[derive(Clone)]
pub struct MonitorTx {
    inner: Option<broadcast::Sender<MonitorEvent>>,
    /// Max bytes captured per HTTP body. `0` disables body capture entirely (metadata only),
    /// bounding the extra memory the monitor holds per event.
    body_limit: usize,
}

impl MonitorTx {
    pub fn disabled() -> Self {
        Self {
            inner: None,
            body_limit: 0,
        }
    }

    pub fn emit(&self, event: MonitorEvent) {
        if let Some(tx) = &self.inner {
            let _ = tx.send(event);
        }
    }

    pub fn from_sender(tx: broadcast::Sender<MonitorEvent>) -> Self {
        Self::from_sender_with_body_limit(tx, BODY_CAPTURE_LIMIT)
    }

    pub fn from_sender_with_body_limit(
        tx: broadcast::Sender<MonitorEvent>,
        body_limit: usize,
    ) -> Self {
        Self {
            inner: Some(tx),
            body_limit,
        }
    }

    /// Returns a fresh [`BodyCapture`] honoring the configured limit, or [`None`] when body
    /// capture is disabled (limit `0`) so emit sites skip the accumulation entirely.
    pub fn new_body_capture(&self) -> Option<BodyCapture> {
        (self.body_limit > 0).then(|| BodyCapture::new(self.body_limit))
    }

    pub fn subscribe(&self) -> Option<broadcast::Receiver<MonitorEvent>> {
        self.inner.as_ref().map(|tx| tx.subscribe())
    }
}
