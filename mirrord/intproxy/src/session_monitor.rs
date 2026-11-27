use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

pub mod api;
pub mod chaos;
pub(crate) mod replay;

use replay::ReplayStore;

/// Wrapper around `Vec<String>` that redacts its [`Debug`] output to avoid leaking environment
/// variable names into logs, while still serializing normally for the session monitor API.
#[derive(Clone, Deserialize, Serialize)]
#[serde(transparent)]
pub struct RedactedVarNames(pub Vec<String>);

impl core::fmt::Debug for RedactedVarNames {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_tuple("RedactedVarNames")
            .field(&"<REDACTED>")
            .finish()
    }
}

/// Shared event schema keeps the session monitor producer and UI server's event handling typed.
#[derive(Clone, Debug, Deserialize, Serialize)]
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
        /// The chaos rule that targets this connection, if there is one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chaos_rule: Option<uuid::Uuid>,
    },
    PortSubscription {
        port: u16,
        mode: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        hit_count: Option<u64>,
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

/// The session monitor as the proxies and the session API use it: where they send and receive
/// [`MonitorEvent`]s, and the [`ReplayStore`] that they share.
///
/// When the session monitor is disabled, this wraps `None`: all emit calls are no-ops and no
/// request is captured.
#[derive(Clone)]
pub struct MonitorTx {
    inner: Option<Enabled>,
}

/// What an enabled session monitor holds.
#[derive(Clone)]
struct Enabled {
    event_tx: broadcast::Sender<MonitorEvent>,
    replay_store: ReplayStore,
}

impl MonitorTx {
    pub fn disabled() -> Self {
        Self { inner: None }
    }

    pub fn emit(&self, event: MonitorEvent) {
        if let Some(enabled) = &self.inner {
            let _ = enabled.event_tx.send(event);
        }
    }

    /// Creates an enabled session monitor with a store of captured requests of its own.
    ///
    /// To share the store, clone the returned value. The proxies and the session API of one
    /// session must share it, or the API would never see the requests that the proxies capture.
    pub fn from_sender(tx: broadcast::Sender<MonitorEvent>) -> Self {
        Self {
            inner: Some(Enabled {
                event_tx: tx,
                replay_store: ReplayStore::default(),
            }),
        }
    }

    /// Returns the store of captured requests. [`None`] when the session monitor is disabled.
    pub(crate) fn replay_store(&self) -> Option<&ReplayStore> {
        self.inner.as_ref().map(|enabled| &enabled.replay_store)
    }

    pub fn subscribe(&self) -> Option<broadcast::Receiver<MonitorEvent>> {
        self.inner
            .as_ref()
            .map(|enabled| enabled.event_tx.subscribe())
    }
}
