use std::time::Duration;

use mirrord_config::feature::network::filter::AddressFilter;
use mirrord_protocol::tcp::HttpFilter;
use regex::RegexSet;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::broadcast;
use uuid::Uuid;

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

pub mod api;

// might have to move all this into the ui somewhere? especially creating a rule from json

// priority: Conflict resolution when multiple rules match the same request. Higher number wins; the
// matching rule with the highest priority applies and others are skipped. Defaults to 0.
#[allow(unused)] // TODO: remove
pub struct ChaosRule {
    pub id: Uuid,
    pub name: Option<String>,
    pub priority: usize,
    pub selector: ChaosSelector, // selector contains the effect
    pub hit_count: usize,
}

impl Default for ChaosRule {
    // Avoid using `#[derive(Default)]` on `ChaosRule` -> Uuid::Default() gives Uuid::nil(), which
    // is just zero.
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            name: Default::default(),
            priority: Default::default(),
            selector: Default::default(),
            hit_count: Default::default(),
        }
    }
}

// example rule request
// nb: requests dont contain the rule id
//
// {
//   "name": "config-read-slow",
//   "effect": {
//     "latency": {
//       "delay_ms": 200,
//       "jitter_ms": 50,
//     },
//     "selector": {
//       "file_path": "/etc/config/*"
//     }
//   }
// }

impl TryFrom<Value> for ChaosRuleRequest {
    type Error = ();

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        // assert this is a single rule
        if !value.is_object() {
            return Err(());
        }
        // top level: name, effect, selector, priority
        value.get("name");
        todo!()
    }
}

// TODO: move this closer to endpoint the user curls
/// Exists to match the structure of a rule request from POST requests, this rule is unvalidated. In
/// converting to ChaosRule, the rule becomes validated.
pub struct ChaosRuleRequest {
    pub name: Option<()>,
    pub priority: Option<()>,
    pub effect: ChaosEffectRequest,
    pub selector: ChaosSelectorRequest,
}

impl TryFrom<ChaosRuleRequest> for ChaosRule {
    type Error = ();

    fn try_from(value: ChaosRuleRequest) -> Result<Self, Self::Error> {
        // TODO: determine selector type from fields
        todo!()
    }
}

// can use mirrord_config::feature::network::filter::AddressFilter
// then filter.matches() (like in the layer)
#[derive(Default)]
pub enum ChaosSelector {
    Tcp {
        upstream: AddressFilter, // req
        percentage: Percentage,
        effect: TcpChaosEffect,
    },
    Http {
        upstream: AddressFilter, // req
        percentage: Percentage,
        filter: HttpFilter, // ::Body and ::HeaderJq variants unused
        effect: HttpChaosEffect,
    },
    Fs {
        file_path: RegexSet, // req
        percentage: Percentage,
        effect: FsChaosEffect,
    },
    #[default]
    None,
}

// may be able to use strum discriminants to convert between string and enum variants without
// matching on strings

// having separate enums per selector allows invalid effect/ selector combos to
// be impossible
#[derive(Default)]
pub enum TcpChaosEffect {
    Latency(ChaosEffectLatency),
    ConnectionError(ChaosEffectConnError),
    Degradation,
    #[default]
    Nothing,
}

#[derive(Default)]
pub enum HttpChaosEffect {
    Latency(ChaosEffectLatency),
    HttpOverride,
    #[default]
    Nothing,
}

#[derive(Default)]
pub enum FsChaosEffect {
    Latency(ChaosEffectLatency),
    FsError,
    #[default]
    Nothing,
}

pub struct ChaosEffectLatency {
    pub delay: Duration, // req
    pub jitter: Duration,
}

pub struct ChaosEffectConnError {
    pub error_type: ConnErrorType, // req
    pub delay: Duration,
}

enum ConnErrorType {
    Reset,   // TCP RST
    Timeout, // hangs then closes
    Refused, // ECONNREFUSED
}

// Helper type for a number between 0 and 100 inclusive. Defaults to 100
#[derive(Clone)]
pub struct Percentage(usize);

impl Percentage {
    pub fn as_percentage(&self) -> usize {
        self.0
    }

    pub fn as_decimal(&self) -> f32 {
        self.0 as f32 / 100.
    }
}

impl From<usize> for Percentage {
    fn from(value: usize) -> Self {
        Self(value.min(100))
    }
}

impl From<f32> for Percentage {
    fn from(value: f32) -> Self {
        (value / 100.).into()
    }
}

impl Default for Percentage {
    fn default() -> Self {
        Self(100)
    }
}
