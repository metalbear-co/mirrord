use std::{fmt::Display, str::FromStr, time::Duration};

use anyhow::{Context, anyhow};
use mirrord_config::feature::network::filter::AddressFilter;
use mirrord_intproxy_protocol::NetProtocol;
use mirrord_protocol::{outgoing::SocketAddress, tcp::HttpFilter};
use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;
use strum_macros::EnumString;
use thiserror::Error;
use uuid::Uuid;

use crate::session_monitor::chaos::*;

/// A valid chaos rule created from a user request ([`ChaosRuleRequest`]) in order to perform chaos
/// testing via fault injection.
///
/// Chaos rules are used in the [`IntProxy`](crate::IntProxy) to purposefully inject faults into
/// traffic coming from sources external to the user's process. They exist locally and are attached
/// and applied to a single `mirrord` session. When outgoing traffic matches a rule, the `effect`
/// specified by the user is applied, for example artificial latency or a connection error.
///
/// Rules can be applied to TCP, HTTP or FS traffic with a given percentage probability. The traffic
/// type is inferred from the requested `selector`.
///
/// For examples of requests and the rules they correspond to, see [`mod test`].
///
/// _WARNING: This type implements `PartialEq` for testing purposes - trying to compare rules
/// without accounting for their unique `id`s will fail!_
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    #[serde(with = "atomic_u32_arc")]
    pub hit_count: Arc<AtomicU32>,
}

impl ChaosRule {
    /// Creates a new [`Self`](ChaosRule) with a new [`Uuid`]. If @name is `None`, it will be added
    /// as `None` and skipped when serializing. If @priority is `None`, it will default to 0
    /// also be skipped when serializing.
    pub fn new(name: Option<String>, priority: Option<usize>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name,
            priority: priority.unwrap_or_default(),
            ..Default::default()
        }
    }

    #[tracing::instrument(level = Level::INFO, ret)]
    pub fn applies_to_address(
        &self,
        remote_address: &SocketAddress,
        protocol: NetProtocol,
        remote_hostname: Option<&String>,
    ) -> bool {
        match &self.selector {
            // TODO(alex) [high]: We need to resolve the `AddressFilter::Name(name, port)` to an ip,
            // so we can compare it with the `remote_address`.
            ChaosSelector::Tcp { upstream, .. } => {
                upstream.matches_socket_address(remote_address, remote_hostname)
                    && matches!(protocol, NetProtocol::Stream)
            }

            ChaosSelector::Http { .. } | ChaosSelector::Fs { .. } | ChaosSelector::None => false,
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

impl PartialEq for ChaosRule {
    fn eq(
        &self,
        ChaosRule {
            id: _id,
            name,
            priority,
            selector,
            hit_count: _hit_count,
        }: &Self,
    ) -> bool {
        // note: don't use .. to refer to fields in `ChaosRule` when matching, otherwise any new
        // fields that are added will be silently ignored when comparing for equality. Explicity
        // ignore fields that shouldn't be compared
        self.name.eq(name) && self.priority.eq(priority) && self.selector.eq(selector)
    }
}

impl Eq for ChaosRule {}

impl Hash for ChaosRule {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state);
        self.name.hash(state);
        self.priority.hash(state);
        self.selector.hash(state);
    }
}

impl Borrow<Uuid> for ChaosRule {
    fn borrow(&self) -> &Uuid {
        &self.id
    }
}

#[derive(Debug, Error)]
pub enum ChaosRuleError {
    #[error("the chaos rule request was invalid: {0}")]
    Invalid(anyhow::Error),

    #[error("this feature is not yet available in mirrord chaos: {0}")]
    Unimplemented(String),
}

impl TryFrom<ChaosEffectRequest> for TcpChaosEffect {
    type Error = ChaosRuleError;

    fn try_from(value: ChaosEffectRequest) -> Result<Self, Self::Error> {
        match value {
            ChaosEffectRequest::Latency {
                delay_ms,
                jitter_ms,
            } => Ok(Self::Latency(ChaosEffectLatency {
                delay: Duration::from_millis(delay_ms),
                jitter: jitter_ms.map(Duration::from_millis).unwrap_or_default(),
            })),

            ChaosEffectRequest::ConnectionError {
                error_type,
                after_ms,
            } => Ok(Self::ConnectionError(ChaosEffectConnError {
                error_type: ConnErrorType::from_str(&error_type)
                    .context("unknown value for 'effect.connection_error.type'")
                    .map_err(ChaosRuleError::Invalid)?,
                after: after_ms.map(Duration::from_millis).unwrap_or_default(),
            })),

            other => Err(ChaosRuleError::Unimplemented(format!("{other:?} effect"))),
        }
    }
}

impl TryFrom<(ChaosSelectorRequest, ChaosEffectRequest)> for ChaosSelector {
    type Error = ChaosRuleError;

    fn try_from(
        (selector, effect): (ChaosSelectorRequest, ChaosEffectRequest),
    ) -> Result<Self, Self::Error> {
        let percentage = selector
            .percentage
            .map(Percentage::from)
            .unwrap_or_default();

        match selector {
            ChaosSelectorRequest {
                upstream: Some(upstream),
                file_path: None,
                header_filter: None,
                path_filter: None,
                method_filter: None,
                all_of: None,
                any_of: None,
                percentage: _,
            } => Ok(Self::Tcp {
                upstream: AddressFilter::from_str(&upstream)
                    .context("failed to parse requested 'selector.upstream' into an address")
                    .map_err(ChaosRuleError::Invalid)?,
                percentage,
                effect: TcpChaosEffect::try_from(effect)?,
            }),

            ChaosSelectorRequest {
                upstream: Some(_),
                file_path: None,
                header_filter: Some(_),
                path_filter: Some(_),
                method_filter: Some(_),
                all_of: Some(_),
                any_of: Some(_),
                percentage: _,
            } => Err(ChaosRuleError::Unimplemented("HTTP selector".to_owned())),

            ChaosSelectorRequest {
                upstream: None,
                file_path: Some(_),
                header_filter: None,
                path_filter: None,
                method_filter: None,
                all_of: None,
                any_of: None,
                percentage: _,
            } => Err(ChaosRuleError::Unimplemented("FS selector".to_owned())),

            _ => Err(anyhow!(
                "couldn't derive a protocol type from fields in `selector` request"
            ))
            .map_err(ChaosRuleError::Invalid),
        }
    }
}

impl TryFrom<ChaosRuleRequest> for ChaosRule {
    type Error = ChaosRuleError;

    fn try_from(
        ChaosRuleRequest {
            name,
            priority,
            effect,
            selector,
        }: ChaosRuleRequest,
    ) -> Result<Self, Self::Error> {
        Ok(Self {
            id: Uuid::new_v4(),
            name,
            priority: priority.unwrap_or_default(),
            selector: ChaosSelector::try_from((selector, effect))?,
            hit_count: Arc::new(AtomicU32::default()),
        })
    }
}

/// Represents a rule request from POST requests, corresponding to a rule that is not yet validated.
/// In converting [`Self`](ChaosRuleRequest) to a [`ChaosRule`], the rule becomes validated.
#[skip_serializing_none]
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct ChaosRuleRequest {
    /// Optional label specified by the user to identify the rule. Internally, the rule ID is used
    /// to differentiate between rules, so `name` uniqueness is not required.
    pub name: Option<String>,

    /// Optional integer used to choose which rule to apply when multiple rules match the same
    /// request. Only the rule with the highest `priority` value is applied. If not given, defaults
    /// to 0 (lowest priority).
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
#[skip_serializing_none]
#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ChaosEffectRequest {
    Latency {
        delay_ms: u64,
        jitter_ms: Option<u64>,
    },
    ConnectionError {
        #[serde(rename = "type")]
        error_type: String,
        after_ms: Option<u64>,
    },
    Degradation,
    HttpOverride,
    FsError,
}

/// The traffic to which a [`ChaosRule`] should apply. The (protocol) type of selector (see
/// [`ChaosSelector`] variants) is inferred from the combination of fields in the request. Either
/// `upstream` or `file_path` is required. The `percentage` can be specified for any protocol.
#[skip_serializing_none]
#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ChaosSelectorRequest {
    /// The target of the rule. Uses the same syntax via [`AddressFilter`] as `mirrord_config`'s
    /// [`OutgoingFilterConfig`](mirrord_config::feature::network::outgoing::OutgoingFilterConfig).
    upstream: Option<String>,

    /// File path patterns for the rule to target. Uses the same syntax as
    /// [`FsConfig`](mirrord_config::feature::fs::advanced).
    file_path: Option<()>,

    // these fields get turned into ChaosSelector::Http.filter ie. HttpFilter
    header_filter: Option<()>,
    path_filter: Option<()>,
    method_filter: Option<()>,
    all_of: Option<()>,
    any_of: Option<()>,

    /// The chance of a rule being applied to matching traffic. Roughly equal to the proportion of
    /// requests that the rule is applied to. Should be an integer between 0 and 100 (values higher
    /// than 100 will be rounded down to 100).
    percentage: Option<usize>,
}

impl ChaosSelectorRequest {
    pub fn tcp_port(port: u16, percentage: Option<usize>) -> Self {
        Self {
            upstream: Some(format!(":{port}")),
            percentage,
            ..Default::default()
        }
    }

    pub fn name() -> Self {
        Self {
            upstream: Some(format!("google.com:443")),
            ..Default::default()
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Default, Debug, PartialEq, Hash)]
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
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Hash, Default)]
pub enum TcpChaosEffect {
    Latency(ChaosEffectLatency),
    ConnectionError(ChaosEffectConnError),
    Degradation,
    #[default]
    Nothing,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Hash, Default)]
pub enum HttpChaosEffect {
    Latency(ChaosEffectLatency),
    HttpOverride,
    #[default]
    Nothing,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Hash, Default)]
pub enum FsChaosEffect {
    Latency(ChaosEffectLatency),
    FsError,
    #[default]
    Nothing,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Hash)]
pub struct ChaosEffectLatency {
    pub delay: Duration, // req
    pub jitter: Duration,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Hash)]
pub struct ChaosEffectConnError {
    pub error_type: ConnErrorType, // req
    pub after: Duration,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Hash, EnumString)]
#[strum(ascii_case_insensitive)]
pub enum ConnErrorType {
    Reset,   // TCP RST
    Timeout, // hangs then closes
    Refused, // ECONNREFUSED
}

/// Helper type for a number between 0 and 100 inclusive. Defaults to 100%. Values larger than 100%
/// get rounded down to 100%.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Hash)]
pub struct Percentage(usize);

impl Percentage {
    pub fn new(value: u32) -> Self {
        Self(value.max(100) as usize)
    }

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
        Self(value.min(1.) as usize * 100)
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

    use crate::session_monitor::chaos::rules::{
        ChaosEffectConnError, ChaosEffectLatency, ChaosEffectRequest, ChaosRule, ChaosRuleRequest,
        ChaosSelector, ChaosSelectorRequest, ConnErrorType, Percentage, TcpChaosEffect,
    };

    /// A helper function that returns a [`ChaosRule`] the same as `@rule` with the `id` set to 0
    /// for comparing rules. This means newly added fields won't be missed in the comparison.
    fn no_id(rule: ChaosRule) -> ChaosRule {
        ChaosRule {
            id: Uuid::nil(),
            ..rule
        }
    }

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
            ..Default::default()
        }
    }, ChaosRule {
        id: Uuid::default(),
        name: Some("rust-connect-slow".to_owned()),
        selector: ChaosSelector::Tcp {
            upstream: AddressFilter::Name("rust-lang.org".to_owned(), 0),
            percentage: Percentage::from(100),
            effect: TcpChaosEffect::Latency(ChaosEffectLatency {
                delay: Duration::from_millis(200),
                jitter: Duration::from_millis(50),
            }),
        },
        ..Default::default()
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
            percentage: Some(75),
            ..Default::default()
        }
    }, ChaosRule {
        id: Uuid::default(),
        priority: 100,
        selector: ChaosSelector::Tcp {
            upstream: AddressFilter::Name("rust-lang.org".to_owned(), 0),
            percentage: Percentage::from(75),
            effect: TcpChaosEffect::ConnectionError(ChaosEffectConnError {
                error_type: ConnErrorType::Timeout,
                after: Duration::from_millis(750)
            }),
        },
        ..Default::default()
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
        assert_eq!(no_id(validated_rule), no_id(expected_validated_rule));
    }

    #[rstest]
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
            percentage: Some(20),
            ..Default::default()
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
            percentage: Some(40),
            ..Default::default()
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
        effect: ChaosEffectRequest::ConnectionError {
            error_type: "timeout".to_owned(),
            after_ms: Some(750)
        },
        selector: ChaosSelectorRequest {
            upstream: Some("rust-lang.org".to_owned()),
            percentage: Some(75),
            ..Default::default()
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
            percentage: Some(75),
            ..Default::default()
        }
    })]
    #[should_panic]
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
    #[should_panic]
    fn error_on_parse_malformed_request(#[case] invalid_rule_req: serde_json::Value) {
        let _: ChaosRuleRequest = serde_json::from_str(&invalid_rule_req.to_string()).unwrap();
    }
}
