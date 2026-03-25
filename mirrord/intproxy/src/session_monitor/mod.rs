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

#[cfg(unix)]
pub mod api;
