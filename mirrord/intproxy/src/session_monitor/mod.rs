use serde::Serialize;
use tokio::sync::broadcast;

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
    HttpRequest {
        method: String,
        path: String,
        host: String,
        port: u16,
    },
    IncomingRequest {
        method: String,
        path: String,
        host: String,
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
        vars: Vec<String>,
    },
    LayerConnected {
        pid: u32,
        process_name: String,
    },
    LayerDisconnected {
        pid: u32,
    },
}

/// Wrapper around an optional broadcast sender for session monitor events.
///
/// When the session monitor is disabled, this wraps `None` and all emit calls are no-ops.
#[derive(Clone)]
pub struct MonitorTx {
    inner: Option<broadcast::Sender<MonitorEvent>>,
}

impl MonitorTx {
    pub fn disabled() -> Self {
        Self { inner: None }
    }

    pub fn emit(&self, event: MonitorEvent) {
        if let Some(tx) = &self.inner {
            let _ = tx.send(event);
        }
    }

    pub fn from_sender(tx: broadcast::Sender<MonitorEvent>) -> Self {
        Self { inner: Some(tx) }
    }

    pub fn subscribe(&self) -> Option<broadcast::Receiver<MonitorEvent>> {
        self.inner.as_ref().map(|tx| tx.subscribe())
    }
}

/// Tries to parse an HTTP/1.x request from raw bytes.
/// Returns `(method, path, host)` on success.
pub fn try_parse_http_request(bytes: &[u8]) -> Option<(String, String, String)> {
    const HTTP_METHODS: &[&str] = &[
        "GET", "POST", "PUT", "DELETE", "HEAD", "OPTIONS", "PATCH", "CONNECT", "TRACE",
    ];

    let s = std::str::from_utf8(bytes).ok()?;
    let first_line_end = s.find("\r\n")?;
    let first_line = &s[..first_line_end];

    let mut parts = first_line.splitn(3, ' ');
    let method = parts.next()?;
    let path = parts.next()?;
    let version = parts.next()?;

    if !HTTP_METHODS.contains(&method) {
        return None;
    }
    if !version.starts_with("HTTP/") {
        return None;
    }

    let host_line = s[first_line_end + 2..]
        .lines()
        .find(|l| l.len() > 5 && l[..5].eq_ignore_ascii_case("host:"))?;
    let host = host_line[5..].trim();

    Some((method.to_owned(), path.to_owned(), host.to_owned()))
}

#[cfg(unix)]
pub mod api;
