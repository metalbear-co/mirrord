use std::{str::FromStr, time::Duration};

use anyhow::error;
use mirrord_config::feature::network::filter::AddressFilter;
use mirrord_protocol::tcp::HttpFilter;
use serde::{Deserialize, Serialize};
use strum_macros::EnumString;
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
#[derive(Debug)]
pub struct ChaosRule {
    pub id: Uuid,
    pub name: Option<String>,
    pub priority: usize,
    pub selector: ChaosSelector, // selector contains the effect
    pub hit_count: usize,
}

impl ChaosRule {
    pub fn new(name: Option<String>, priority: Option<usize>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name,
            priority: priority.unwrap_or_default(),
            ..Default::default()
        }
    }
}

impl Default for ChaosRule {
    /// Avoid using `#[derive(Default)]` on `ChaosRule` -> Uuid::Default() gives Uuid::nil(), which
    /// is just zero.
    ///
    /// ```
    /// // equivalent to calling ChaosRule::default()
    /// let default_rule = ChaosRule {
    ///     id: Uuid::new_v4(),
    ///     name: None,
    ///     priority: 0,
    ///     selector: ChaosSelector::None,
    ///     hit_count: 0,
    /// };
    /// ```
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

impl TryFrom<ChaosRuleRequest> for ChaosRule {
    type Error = anyhow::Error;

    fn try_from(value: ChaosRuleRequest) -> Result<Self, Self::Error> {
        // start with name, priority, a new UUID (from `impl Default`) and default values for
        // selector, effect and hit_count
        let mut rule = ChaosRule::new(value.name, value.priority);

        // infer selector type from which fields were set
        // see `ChaosSelector` variants for which fields each selector can have.
        let percentage = value
            .selector
            .percentage
            .map(Percentage::from)
            .unwrap_or_default();
        rule.selector = match value.selector {
            ChaosSelectorRequest {
                upstream: Some(upstream),
                percentage: _,
            } => {
                let upstream = AddressFilter::from_str(&upstream)
                    .context("failed to parse 'selector.upstream' into an address")?;

                ChaosSelector::Tcp {
                    upstream,
                    percentage,
                    effect: TcpChaosEffect::Nothing,
                }
            }
            _ => panic!("invalid selector, couldn't derive a type"),
        };

        // check the effect and selector are compatible
        match rule.selector {
            ChaosSelector::Tcp { ref mut effect, .. } => {
                *effect = match value.effect {
                    ChaosEffectRequest::Latency {
                        delay_ms,
                        jitter_ms,
                    } => TcpChaosEffect::Latency(ChaosEffectLatency {
                        delay: Duration::from_millis(delay_ms),
                        jitter: jitter_ms.map(Duration::from_millis).unwrap_or_default(),
                    }),
                    ChaosEffectRequest::ConnectionError {
                        error_type,
                        after_ms,
                    } => TcpChaosEffect::ConnectionError(ChaosEffectConnError {
                        error_type: ConnErrorType::from_str(&error_type)?,
                        after: after_ms.map(Duration::from_millis).unwrap_or_default(),
                    }),
                };
            }
            ChaosSelector::Http { .. } | ChaosSelector::Fs { .. } | ChaosSelector::None => {
                unimplemented!()
            }
        };

        Ok(rule)
    }
}

/// Exists to match the structure of a rule request from POST requests, this rule is unvalidated. In
/// converting to a ChaosRule, the rule becomes validated.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct ChaosRuleRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<usize>,
    pub effect: ChaosEffectRequest, // enum of like Latency {thing: type, thign2: type}
    pub selector: ChaosSelectorRequest,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ChaosEffectRequest {
    Latency {
        delay_ms: u64, // req
        #[serde(skip_serializing_if = "Option::is_none")]
        jitter_ms: Option<u64>,
    },
    ConnectionError {
        #[serde(rename = "type")]
        error_type: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        after_ms: Option<u64>,
    },
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct ChaosSelectorRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    upstream: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    percentage: Option<usize>,
}

// can use mirrord_config::feature::network::filter::AddressFilter
// then filter.matches() (like in the layer)
#[derive(Default, Debug, PartialEq)]
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
        file_path: Vec<String>, // req
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
#[derive(Default, Debug, PartialEq)]
pub enum TcpChaosEffect {
    Latency(ChaosEffectLatency),
    ConnectionError(ChaosEffectConnError),
    Degradation,
    #[default]
    Nothing,
}

#[derive(Default, Debug, PartialEq)]
pub enum HttpChaosEffect {
    Latency(ChaosEffectLatency),
    HttpOverride,
    #[default]
    Nothing,
}

#[derive(Default, Debug, PartialEq)]
pub enum FsChaosEffect {
    Latency(ChaosEffectLatency),
    FsError,
    #[default]
    Nothing,
}

#[derive(Debug, PartialEq)]
pub struct ChaosEffectLatency {
    pub delay: Duration, // req
    pub jitter: Duration,
}

#[derive(Debug, PartialEq)]
pub struct ChaosEffectConnError {
    pub error_type: ConnErrorType, // req
    pub after: Duration,
}

#[derive(Debug, PartialEq, EnumString)]
#[strum(ascii_case_insensitive)]
pub enum ConnErrorType {
    Reset,   // TCP RST
    Timeout, // hangs then closes
    Refused, // ECONNREFUSED
}

// Helper type for a number between 0 and 100 inclusive. Defaults to 100
#[derive(Clone, Debug, PartialEq)]
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
