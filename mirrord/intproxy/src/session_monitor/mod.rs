use std::{
    borrow::Borrow,
    collections::HashSet,
    fmt::Display,
    hash::Hasher,
    ops::Deref,
    str::FromStr,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};

use anyhow::Context;
use mirrord_config::feature::network::filter::AddressFilter;
use mirrord_protocol::tcp::HttpFilter;
use regex::RegexSet;
use serde::{Deserialize, Deserializer, Serialize, Serializer, ser::SerializeStruct};
use serde_json::Value;
use strum_macros::EnumString;
use tokio::sync::{
    broadcast,
    watch::{self, error::SendError},
};
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

/// A valid chaos rule created from a user request ([`ChaosRuleRequest`]) in order to perform chaos
/// testing via fault injection.
///
/// Chaos rules are used in the [`IntProxy`](crate::IntProxy) to purposefully inject faults into
/// traffic coming from sources external to the user's process. They exist locally and are attached
/// and applied to a single `mirrord` session. When outgoing traffic matches a rule, the `effect`
/// specified by the user is applied, for example artificial latency or a connection error.
///
/// Rules can be applied to TCP, HTTP or Fs traffic with a given percentage probability. The traffic
/// type is inferred from the requested `selector`.
///
/// For examples of requests and the rules they correspond to, see [`mod test`].
#[derive(Debug)]
pub struct ChaosRule {
    /// The UUID of the rule (unrelated to the session ID that the rule is attached to). Created
    /// with [`Uuid::new_v4()`] via [`Self::new()`] or [`Self::default()`]. Cannot be specified
    /// by the user.
    pub id: Uuid,

    /// Optional label specified by the user to identify the rule. If no name is given, defaults to
    /// `None`.
    pub name: Option<String>,

    /// An integer used to choose which rule to apply when multiple rules match the same request.
    /// Only the rule with the highest priority is applied. If not specified by the user, defaults
    /// to 0 (lowest priority).
    pub priority: usize,

    /// The selector determines what type of traffic to apply the rule to, and to filter that
    /// traffic further if required. It also contains the effect that the rule applies (this is how
    /// we enforce selector-effect compatibility - a [TCP-only effect](TcpChaosEffect) cannot exist
    /// in an [HTTP selector](ChaosSelector::Http), for example).
    ///
    /// In user requests, the `selector` and `effect` fields are separate, and we combine them
    /// during validation in [`Self::try_from<ChaosRuleRequest>()`].
    pub selector: ChaosSelector,

    /// The number of times the rule has been applied, modified by the tasks where the rule is
    /// applied. Cannot be specified by the user, starts at zero.
    pub hit_count: usize,
}

impl ChaosRule {
    /// Creates a new [`Self`](ChaosRule) with a new [`Uuid`]. If @name is `None`, it will be added
    /// as `None` and skipped when serializing. If @priority is `None`, it will default to 0
    /// also be skipped when serializing.
    pub fn new(name: Option<String>, priority: Option<usize>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name,
            priority: priority.unwrap_or_default(),
            ..Default::default()
        }
    }
}

// Note for devs: Avoid implementing `#[derive(Default)]` on `ChaosRule`: `Uuid::default()`
// gives `Uuid::nil()`, which is just zero and will lead to conflicts.
impl Default for ChaosRule {
    /// Use [`Self::new()`] instead.
    ///
    /// ```
    /// let default_rule = ChaosRule::default();
    /// // equivalent to calling ChaosRule::default()
    /// let expected = ChaosRule {
    ///     id: default_rule.id, // Uuid::new_v4()
    ///     name: None,
    ///     priority: 0,
    ///     selector: ChaosSelector::None,
    ///     hit_count: 0,
    /// };
    /// assert_eq!(default_rule, expected);
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

    /// Create a new [`Self`](ChaosRule) from a [`ChaosRuleRequest`], verifying that the rule is
    /// valid. The selector must be inferred from the fields in `@value.selector`, and then checked
    /// for compatibility with the requested effect type in `@value.effect`.
    ///
    /// Will fail for unimplemented effects and selectors.
    fn try_from(value: ChaosRuleRequest) -> Result<Self, Self::Error> {
        // start with name, priority, a new UUID (from `impl Default`) and default values for
        // selector, effect and hit_count
        let mut rule = ChaosRule::new(value.name, value.priority);

        // infer selector type from which fields were set
        // see `ChaosSelector` variants for which fields each selector can have
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
                    .context("failed to parse requested 'selector.upstream' into an address")?;

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
                unimplemented!("HTTP and Fs selectors are not yet available for chaos rules")
            }
        };

        Ok(rule)
    }
}

/// Represents a rule request from POST requests, corresponding to a rule that is not yet validated.
/// In converting [`Self`](ChaosRuleRequest) to a [`ChaosRule`], the rule becomes validated.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct ChaosRuleRequest {
    /// Optional label specified by the user to identify the rule. Internally, the rule ID is used
    /// to differentiate between rules, so `name` uniqueness is not required.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// Optional integer used to choose which rule to apply when multiple rules match the same
    /// request. Only the rule with the highest `priority` value is applied. If not given, defaults
    /// to 0 (lowest priority).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<usize>,

    /// The type of effect that the rule should apply. Should only be used with a compatible
    /// `selector`, or rule creation will fail.
    pub effect: ChaosEffectRequest,

    /// The traffic to which the rule should apply. Should only be used with a compatible `effect`,
    /// or rule creation will fail. The type of selector is inferred from which fields are in
    /// the request.
    pub selector: ChaosSelectorRequest,
}

/// The type of effect that a [`ChaosRule`] should apply. Can only be used with a compatible
/// `selector`, as checked in [`ChaosRule::try_from<ChaosRuleRequest>()`].
#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ChaosEffectRequest {
    Latency {
        delay_ms: u64,
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

/// The traffic to which a [`ChaosRule`] should apply. The (protocol) type of selector (see
/// [`ChaosSelector`] variants) is inferred from the combination of fields in the request. Either
/// `upstream` or `file_path` is required. The `percentage` can be specified for any protocol.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct ChaosSelectorRequest {
    /// The target of the rule. Uses the same syntax via [`AddressFilter`] as `mirrord_config`'s
    /// [`OutgoingFilterConfig`](mirrord_config::feature::network::outgoing::OutgoingFilterConfig).
    #[serde(skip_serializing_if = "Option::is_none")]
    upstream: Option<String>,

    /// The chance of a rule being applied to matching traffic. Roughly equal to the proportion of
    /// requests that the rule is applied to. Should be an integer between 0 and 100 (values higher
    /// than 100 will be rounded down to 100).
    #[serde(skip_serializing_if = "Option::is_none")]
    percentage: Option<usize>,
}

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

/// Helper type for a number between 0 and 100 inclusive. Defaults to 100%. Values larger than 100%
/// get rounded down to 100%.
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

impl Display for Percentage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}%", self.0)
    }
}

#[cfg(test)]
mod test {
    use std::time::Duration;

    use mirrord_config::feature::network::filter::AddressFilter;
    use rstest::rstest;
    use serde_json::json;
    use uuid::Uuid;

    use crate::session_monitor::{
        ChaosEffectConnError, ChaosEffectLatency, ChaosEffectRequest, ChaosRule, ChaosRuleRequest,
        ChaosSelector, ChaosSelectorRequest, ConnErrorType, Percentage, TcpChaosEffect,
    };

    #[rstest]
    #[case::tcp_latency(json!({
      "name": "rust-connect-slow",
      "effect": {
        "latency": {
          "delay_ms": 200,
          "jitter_ms": 50,
        }
      },
      "selector": {
        "upstream": "rust-lang.org"
      }
    }), ChaosRuleRequest {
        name: Some("rust-connect-slow".to_owned()),
        priority: None,
        effect: ChaosEffectRequest::Latency {
            delay_ms: 200,
            jitter_ms: Some(50)
        },
        selector: ChaosSelectorRequest {
            upstream: Some("rust-lang.org".to_owned()),
            percentage: None
        }
    }, ChaosRule {
        id: Uuid::default(),
        name: Some("rust-connect-slow".to_owned()),
        priority: 0,
        selector: ChaosSelector::Tcp {
            upstream: AddressFilter::Name("rust-lang.org".to_owned(), 0),
            percentage: Percentage::from(100),
            effect: TcpChaosEffect::Latency(ChaosEffectLatency {
                delay: Duration::from_millis(200),
                jitter: Duration::from_millis(50),
            }),
        },
        hit_count: 0,
    })]
    #[case::tcp_conn_error(json!({
        "selector": {
          "upstream": "rust-lang.org",
          "percentage": 75
        },
        "priority": 100,
        "effect": {
          "connection_error": {
            "type": "timeout",
            "after_ms": 750
          }
        }
    }), ChaosRuleRequest {
        name: None,
        priority: Some(100),
        effect: ChaosEffectRequest::ConnectionError {
            error_type: "timeout".to_owned(),
            after_ms: Some(750)
        },
        selector: ChaosSelectorRequest {
            upstream: Some("rust-lang.org".to_owned()),
            percentage: Some(75)
        }
    }, ChaosRule {
        id: Uuid::default(),
        name: None,
        priority: 100,
        selector: ChaosSelector::Tcp {
            upstream: AddressFilter::Name("rust-lang.org".to_owned(), 0),
            percentage: Percentage::from(75),
            effect: TcpChaosEffect::ConnectionError(ChaosEffectConnError {
                error_type: ConnErrorType::Timeout,
                after: Duration::from_millis(750)
            }),
        },
        hit_count: 0,
    })]
    fn parse_valid_request_into_rule(
        #[case] valid_rule_req: serde_json::Value,
        #[case] expected_parsed_type: ChaosRuleRequest,
        #[case] expected_validated_rule: ChaosRule,
    ) {
        let parsed_request: ChaosRuleRequest = serde_json::from_str(&valid_rule_req.to_string())
            .expect("failed deserialization of rule request from valid json");

        assert_eq!(
            parsed_request, expected_parsed_type,
            "json request was turned into a `ChaosRuleRequest`, but it did not match the expected request"
        );

        let validated_rule = ChaosRule::try_from(parsed_request)
            .expect("ChaosRule failed creation from a valid ChaosRuleRequest");

        // we can't compare rules on contents alone since they have unique UUIDs
        assert_eq!(validated_rule.name, expected_validated_rule.name);
        assert_eq!(validated_rule.priority, expected_validated_rule.priority);
        assert_eq!(validated_rule.selector, expected_validated_rule.selector);
        assert_eq!(validated_rule.hit_count, expected_validated_rule.hit_count);
    }

    #[rstest]
    #[should_panic]
    #[case::invalid_selector_too_many_fields(json!({
      "effect": {
        "latency": {
          "delay_ms": 200,
          "jitter_ms": 50,
        }
      },
      "selector": {
        "upstream": "rust-lang.org",
        "file_path": "/mnt/data/*.json",
        "percentage": 20
      }
    }), ChaosRuleRequest {
        name: None,
        priority: None,
        effect: ChaosEffectRequest::Latency {
            delay_ms: 200,
            jitter_ms: Some(50)
        },
        selector: ChaosSelectorRequest {
            upstream: Some("rust-lang.org".to_owned()),
            percentage: Some(20)
        }
    })]
    #[case::invalid_selector_missing_minimum(json!({
      "effect": {
        "latency": {
          "delay_ms": 200,
          "jitter_ms": 50,
        }
      },
      "selector": {
        "percentage": 40
      }
    }), ChaosRuleRequest {
        name: None,
        priority: None,
        effect: ChaosEffectRequest::Latency {
            delay_ms: 200,
            jitter_ms: Some(50)
        },
        selector: ChaosSelectorRequest {
            upstream: None,
            percentage: Some(40)
        }
    })]
    #[case::http_effect_with_tcp_selector(json!({
        "selector": {
          "percentage": 75,
          "upstream": "rust-lang.org"
        },
        "effect": {
          "http_override": {
            "type": "timeout",
            "after_ms": 750
          }
        }
    }), ChaosRuleRequest {
        name: None,
        priority: None,
        effect: ChaosEffectRequest::ConnectionError { // FIX: impl http_override
            error_type: "timeout".to_owned(),
            after_ms: Some(750)
        },
        selector: ChaosSelectorRequest {
            upstream: Some("rust-lang.org".to_owned()),
            percentage: Some(75)
        }
    })]
    #[case::invalid_selector_upstream_address_filter(json!({
        "selector": {
          "upstream": "meow://i-guess-i-could-be-blaze",
          "percentage": 75
        },
        "priority": 100,
        "effect": {
          "connection_error": {
            "type": "timeout",
            "after_ms": 750
          }
        }
    }), ChaosRuleRequest {
        name: None,
        priority: Some(100),
        effect: ChaosEffectRequest::ConnectionError {
            error_type: "timeout".to_owned(),
            after_ms: Some(750)
        },
        selector: ChaosSelectorRequest {
            upstream: Some("rust-lang.org".to_owned()),
            percentage: Some(75)
        }
    })]
    fn parse_well_formed_request_into_invalid_rule(
        #[case] valid_rule_req: serde_json::Value,
        #[case] expected_parsed_type: ChaosRuleRequest,
    ) {
        let parsed_request: ChaosRuleRequest = serde_json::from_str(&valid_rule_req.to_string())
            .expect("failed deserialization of rule request from valid json");

        assert_eq!(
            parsed_request, expected_parsed_type,
            "json request was turned into a `ChaosRuleRequest`, but it did not match the expected request"
        );

        let rule = ChaosRule::try_from(parsed_request).unwrap();
        println!("{rule:?}");
    }

    #[rstest]
    #[should_panic]
    #[case::missing_selector(json!({
      "name" : "the-crocodile-is-called-vector-apparently",
      "effect": {
        "latency": {
          "delay_ms": 200,
          "jitter_ms": 50,
        }
      }
    }))]
    #[case::multiple_effects(json!({
        "selector": {
          "percentage": 75,
          "upstream": "rust-lang.org"
        },
        "effect": {
          "http_override": {
            "type": "timeout",
            "after_ms": 750
          },
          "latency": {
            "delay_ms": 200,
          }
        }
    }))]
    #[case::non_object_effect(json!({
        "selector": {
          "percentage": 75,
          "upstream": "https://sonic.fandom.com/wiki/Team_Chaotix"
        },
        "effect": {
          "http_override": "vector-isn't-a-very-reptilian-name"
        }
    }))]
    fn error_on_parse_malformed_request(#[case] invalid_rule_req: serde_json::Value) {
        let _: ChaosRuleRequest = serde_json::from_str(&invalid_rule_req.to_string()).unwrap();
    }
}
