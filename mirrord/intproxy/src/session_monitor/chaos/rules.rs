//! Defines types that represent chaos rules ([`ChaosRule`]) and rule requests from the user
//! ([`ChaosRuleRequest`]). A [`ChaosRuleRequest`] represents a request that has not yet been
//! validated as a [`ChaosRule`].
//!
//! These types are separate for convenience, so we can change the API and implementation of chaos
//! rules independently of each-other, and to allow for things like selector type
//! ([`ChaosSelectorType`]) inference from requests.

use std::{fmt::Display, str::FromStr, time::Duration};

use anyhow::{Context, anyhow};
use mirrord_config::feature::network::filter::AddressFilter;
use mirrord_intproxy_protocol::NetProtocol;
use mirrord_protocol::{
    ErrorKindInternal, RemoteIOError, outgoing::SocketAddress, tcp::HttpFilter,
};
use rand::{random_bool, random_range};
use serde::{Deserialize, Serialize};
use serde_with::{DisplayFromStr, serde_as, skip_serializing_none};
use strum_macros::{Display, EnumDiscriminants, EnumString};
use thiserror::Error;
use uuid::Uuid;

use crate::session_monitor::chaos::*;

/// A valid chaos rule created from a user request ([`ChaosRuleRequest`]) in order to perform chaos
/// testing via fault injection.
///
/// Chaos rules are used in the [`IntProxy`](crate::IntProxy) to purposefully inject faults into
/// traffic coming from sources external to the user's process. They exist locally and are attached
/// and applied to a single `mirrord` session. When outgoing traffic matches a rule, the
/// [`ChaosEffectType`] specified by the user is applied, for example artificial latency or a
/// connection error.
///
/// Rules can be applied to TCP, HTTP or FS traffic with a given percentage probability. The traffic
/// type is inferred from a request's `selector` ([`ChaosSelectorRequest`]).
///
/// For examples of requests and the rules they correspond to, see [`mod test`].
///
/// _WARNING: This type implements `PartialEq` but only compares the `id` fields._
#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChaosRule {
    /// The UUID of the rule (unrelated to the session ID that the rule is attached to). Created
    /// with [`Uuid::new_v4()`] via [`Self::new()`] or [`Self::default()`]. Cannot be specified
    /// by the user.
    ///
    /// We use only `id` to compare rules in `PartialEq`/ `Eq`. Although it is unique for live
    /// rules, it is not necessarily unique among all rules past and present for the current
    /// session.
    pub id: Uuid,

    /// Optional label specified by the user to identify the rule. If no name is given, defaults to
    /// `None` and is not serialized. No guarantee of uniqueness.
    pub name: Option<String>,

    /// An integer used to choose which rule to apply when multiple rules match the same request.
    /// Only the rule with the highest priority is applied. If not specified by the user, defaults
    /// to 0 (lowest priority).
    pub priority: u32,

    /// The selector determines what type of traffic to apply the rule to, and to filter that
    /// traffic further if required. It also contains the effect that the rule applies (this is how
    /// we enforce selector-effect compatibility - a [TCP-only effect](TcpChaosEffect) cannot exist
    /// in an [HTTP selector](ChaosSelector::Http), for example).
    ///
    /// In user requests, the `selector` and `effect` fields are separate, and we combine them
    /// during validation in [`Self::try_from<ChaosRuleRequest>()`].
    pub selector: ChaosSelector,

    /// The number of times the rule has been applied, modified by the tasks where the rule is
    /// applied. Not necessarily equal to the number of times the matched against network traffic.
    ///
    /// Cannot be specified by the user, starts at zero. When a rule is edited via the PUT method,
    /// the id stays the same but the hit_count resets to zero.
    #[serde(with = "atomic_u32_arc")]
    pub hit_count: Arc<AtomicU32>,
}

impl ChaosRule {
    /// Creates a new [`Self`](ChaosRule) with a new [`Uuid`]. If @name is `None`, it will be added
    /// as `None` and skipped when serializing. If @priority is `None`, it will default to 0.
    pub fn new(name: Option<String>, priority: Option<u32>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name,
            priority: priority.unwrap_or_default(),
            ..Default::default()
        }
    }

    /// Returns `true` if the rule `self` applies to `@remote_address`
    pub fn applies_to_address(
        &self,
        remote_address: &SocketAddress,
        protocol: NetProtocol,
        remote_hostname: Option<&String>,
    ) -> bool {
        match &self.selector {
            ChaosSelector::Tcp { upstream, .. } => {
                upstream.matches_socket_address(remote_address, remote_hostname)
                    && matches!(protocol, NetProtocol::Stream)
            }
            unimpl @ ChaosSelector::Http { .. } | unimpl @ ChaosSelector::Fs { .. } => {
                let error = ChaosRuleError::Unimplemented(format!(
                    "{} selector",
                    ChaosSelectorType::from(unimpl)
                ));
                tracing::error!(%error, "unimplemented selector used in chaos rule with id `{}`", self.id);
                false
            }
            ChaosSelector::None => false,
        }
    }

    /// Convenience method to get the `selector.percentage` for the rule `self`.
    pub fn selector_percentage(&self) -> Percentage {
        match self.selector {
            ChaosSelector::Tcp { percentage, .. }
            | ChaosSelector::Http { percentage, .. }
            | ChaosSelector::Fs { percentage, .. } => percentage,
            ChaosSelector::None => Percentage::default(),
        }
    }

    /// Convenience method to get the `selector` (protocol) type for the rule `self`.
    pub fn selector_type(&self) -> ChaosSelectorType {
        match self.selector {
            ChaosSelector::Tcp { .. } => ChaosSelectorType::Tcp,
            ChaosSelector::Http { .. } => ChaosSelectorType::Http,
            ChaosSelector::Fs { .. } => ChaosSelectorType::Fs,
            ChaosSelector::None => ChaosSelectorType::None,
        }
    }

    /// Convenience method to get the chaos `effect` type for the rule `self`.
    pub fn effect_type(&self) -> Option<ChaosEffectType> {
        self.selector.effect_type()
    }
}

/// Note for devs: Avoid implementing `#[derive(Default)]` on `ChaosRule`: `Uuid::default()`
/// gives `Uuid::nil()`, which is just zero and will lead to conflicts.
impl Default for ChaosRule {
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
    fn eq(&self, ChaosRule { id, .. }: &Self) -> bool {
        self.id.eq(id)
    }
}

impl Eq for ChaosRule {}

impl Hash for ChaosRule {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state);
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

    #[error("couldn't parse JSON: {0}")]
    Deserialize(String),
}

impl serde::de::Error for ChaosRuleError {
    fn custom<T: Display>(msg: T) -> Self {
        ChaosRuleError::Deserialize(msg.to_string())
    }
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
            } => Ok(Self::ConnectionError(ChaosEffectConnectionError {
                error_type: ConnectionErrorType::from_str(&error_type)
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
                header_filter: _,
                path_filter: _,
                method_filter: _,
                all_of: _,
                any_of: _,
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

impl TryFrom<(Uuid, ChaosRuleRequest)> for ChaosRule {
    type Error = ChaosRuleError;

    fn try_from(
        (
            rule_id,
            ChaosRuleRequest {
                name,
                priority,
                effect,
                selector,
            },
        ): (Uuid, ChaosRuleRequest),
    ) -> Result<Self, Self::Error> {
        Ok(Self {
            id: rule_id,
            name,
            priority: priority.unwrap_or_default(),
            selector: ChaosSelector::try_from((selector, effect))?,
            hit_count: Arc::new(AtomicU32::default()),
        })
    }
}

/// Represents a rule request from POST and PUT requests, corresponding to a rule that is not yet
/// validated. In converting [`Self`](ChaosRuleRequest) to a [`ChaosRule`], the rule becomes
/// validated.
#[skip_serializing_none]
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct ChaosRuleRequest {
    /// Optional label specified by the user to identify the rule. Internally, the rule ID is used
    /// to differentiate between rules, so `name` uniqueness is not required.
    pub name: Option<String>,

    /// Optional integer used to choose which rule to apply when multiple rules match the same
    /// request. Only the rule with the highest `priority` value is applied. If not given, defaults
    /// to 0 (lowest priority).
    pub priority: Option<u32>,

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
#[derive(Debug, Serialize, Deserialize, PartialEq, EnumDiscriminants)]
#[serde(rename_all = "snake_case")]
#[strum_discriminants(name(ChaosEffectType))]
#[strum_discriminants(derive(EnumString))]
#[repr(u8)]
pub enum ChaosEffectRequest {
    Latency {
        delay_ms: u64,
        jitter_ms: Option<u64>,
    } = 0,
    ConnectionError {
        #[serde(rename = "type")]
        error_type: String,
        after_ms: Option<u64>,
    } = 1,
    #[serde(skip)]
    // Reinstate when required protocol implemented
    Degradation = 2,
    #[serde(skip)]
    // Reinstate when required protocol implemented
    HttpOverride = 3,
    #[serde(skip)]
    // Reinstate when required protocol implemented
    FsError = 4,
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
    file_path: Option<String>,

    // these fields get turned into ChaosSelector::Http.filter ie. HttpFilter
    header_filter: Option<String>,
    path_filter: Option<String>,
    method_filter: Option<String>,
    all_of: Option<String>,
    any_of: Option<String>,

    /// The chance of a rule being applied to matching traffic. Roughly equal to the proportion of
    /// requests that the rule is applied to. Should be an integer between 0 and 100 (values higher
    /// than 100 will be rounded down to 100).
    percentage: Option<u32>,
}

#[serde_as]
#[serde_with::apply(
    Option => #[serde(skip_serializing_if = "Option::is_none")]
)]
#[derive(Clone, Serialize, Deserialize, Default, Debug, PartialEq, Hash, EnumDiscriminants)]
#[strum_discriminants(derive(Serialize, Deserialize, Display))]
#[strum_discriminants(name(ChaosSelectorType))]
#[repr(u8)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChaosSelector {
    Tcp {
        #[serde_as(as = "DisplayFromStr")]
        upstream: AddressFilter, // req
        percentage: Percentage,
        effect: TcpChaosEffect,
    } = 1,
    Http {
        #[serde_as(as = "DisplayFromStr")]
        upstream: AddressFilter, // req
        percentage: Percentage,
        // #[serde(skip_serializing_if = "Option::is_none")]
        filter: Option<HttpFilter>, // ::Body and ::HeaderJq variants unused
        effect: HttpChaosEffect,
    } = 2,
    Fs {
        file_path: Vec<String>, // req
        percentage: Percentage,
        effect: FsChaosEffect,
    } = 3,
    #[default]
    None = 0,
}

impl ChaosSelector {
    /// Convenience method to get the chaos `effect` type for the selector `self`. Conversion via
    /// `String` is done due to `self.effect` having different types in different variants. Returns
    /// an `Option` because there is no `None` variant of `ChaosEffectType`.
    pub(crate) fn effect_type(&self) -> Option<ChaosEffectType> {
        let string = match &self {
            ChaosSelector::Tcp { effect, .. } => effect.to_string(),
            ChaosSelector::Http { effect, .. } => effect.to_string(),
            ChaosSelector::Fs { effect, .. } => effect.to_string(),
            ChaosSelector::None => {
                return None;
            }
        };

        ChaosEffectType::from_str(&string).ok()
    }
}

/// Possible effects for [`ChaosSelector::Tcp`].
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Hash, Default, strum_macros::Display)]
#[serde(rename_all = "snake_case")]
pub enum TcpChaosEffect {
    Latency(ChaosEffectLatency),
    ConnectionError(ChaosEffectConnectionError),
    Degradation,
    #[default]
    #[strum(disabled)]
    Nothing,
}

/// Possible effects for [`ChaosSelector::Http`].
#[derive(
    Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Hash, Default, strum_macros::Display,
)]
#[serde(rename_all = "snake_case")]
pub enum HttpChaosEffect {
    Latency(ChaosEffectLatency),
    HttpOverride,
    #[default]
    #[strum(disabled)]
    Nothing,
}

/// Possible effects for [`ChaosSelector::Fs`].
#[derive(
    Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Hash, Default, strum_macros::Display,
)]
#[serde(rename_all = "snake_case")]
pub enum FsChaosEffect {
    Latency(ChaosEffectLatency),
    FsError,
    #[default]
    #[strum(disabled)]
    Nothing,
}

/// An effect for [`ChaosEffectType::Latency`].
#[serde_as]
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Hash)]
pub struct ChaosEffectLatency {
    #[serde_as(as = "serde_with::DurationMilliSeconds<u64>")]
    #[serde(rename = "delay_ms")]
    delay: Duration,
    #[serde_as(as = "serde_with::DurationMilliSeconds<u64>")]
    #[serde(rename = "jitter_ms")]
    jitter: Duration,
}

impl ChaosEffectLatency {
    pub fn new(delay: Duration, jitter: Duration) -> Self {
        Self { delay, jitter }
    }
    pub fn latency_duration(&self) -> Duration {
        if self.jitter.is_zero() {
            return self.delay;
        }

        self.delay
            + self
                .jitter
                .div_f32(100.)
                .saturating_mul(random_range(0..=100))
    }
}

/// An effect for [`ChaosEffectType::ConnectionError`].
#[serde_as]
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Hash)]
pub struct ChaosEffectConnectionError {
    pub error_type: ConnectionErrorType,
    #[serde_as(as = "serde_with::DurationMilliSeconds<u64>")]
    #[serde(rename = "after_ms")]
    pub after: Duration,
}

/// The type of error to be returned when [`ChaosEffectConnectionError`] is applied. Can be
/// converted to/ from [`ErrorKindInternal`], and from [`RemoteIOError`].
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Hash, EnumString)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case", ascii_case_insensitive)]
pub enum ConnectionErrorType {
    /// Equivalent to [`ErrorKindInternal::ConnectionReset`].
    Reset,
    /// Equivalent to [`ErrorKindInternal::TimedOut`].
    TimedOut,
    /// Equivalent to [`ErrorKindInternal::ConnectionRefused`].
    Refused,
    /// Equivalent to [`ErrorKindInternal::Unknown`].
    Unknown(String),
}

impl From<ErrorKindInternal> for ConnectionErrorType {
    fn from(value: ErrorKindInternal) -> Self {
        match value {
            ErrorKindInternal::ConnectionRefused => Self::Refused,
            ErrorKindInternal::ConnectionReset => Self::Reset,
            ErrorKindInternal::TimedOut => Self::TimedOut,
            other => Self::Unknown(other.to_string()),
        }
    }
}

impl From<ConnectionErrorType> for ErrorKindInternal {
    fn from(value: ConnectionErrorType) -> Self {
        match value {
            ConnectionErrorType::Reset => ErrorKindInternal::ConnectionReset,
            ConnectionErrorType::TimedOut => ErrorKindInternal::TimedOut,
            ConnectionErrorType::Refused => ErrorKindInternal::ConnectionRefused,
            ConnectionErrorType::Unknown(fail) => Self::Unknown(fail),
        }
    }
}

impl From<ConnectionErrorType> for RemoteIOError {
    fn from(error_type: ConnectionErrorType) -> Self {
        let kind = ErrorKindInternal::from(error_type);
        let raw_os_error = kind.raw_os_error();

        match kind.clone() {
            ErrorKindInternal::ConnectionRefused => RemoteIOError { raw_os_error, kind },
            ErrorKindInternal::ConnectionReset => RemoteIOError { raw_os_error, kind },
            ErrorKindInternal::TimedOut => RemoteIOError { raw_os_error, kind },
            _ => RemoteIOError { raw_os_error, kind },
        }
    }
}

/// Helper type for a number between 0 and 100 inclusive. Defaults to 100%. Values larger than 100%
/// get rounded down to 100%. Serializes as the inner `u32`.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Hash)]
#[serde(transparent)]
pub struct Percentage(u32);

impl Percentage {
    pub fn new(value: u32) -> Self {
        Self::from(value)
    }

    pub fn as_percentage(&self) -> u32 {
        self.0
    }

    pub fn as_decimal(&self) -> f32 {
        self.0 as f32 / 100.
    }

    pub fn roll_for_hit(&self) -> bool {
        random_bool(self.as_decimal() as _)
    }
}

impl From<u32> for Percentage {
    fn from(value: u32) -> Self {
        Self(value.clamp(0, 100))
    }
}

impl From<f32> for Percentage {
    fn from(value: f32) -> Self {
        Self::new((value.abs() * 100.0).round() as u32)
    }
}

impl Default for Percentage {
    fn default() -> Self {
        Self::new(100)
    }
}

impl Display for Percentage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}%", self.0)
    }
}

#[cfg(test)]
mod test {
    use std::{
        collections::hash_map::DefaultHasher,
        hash::{Hash, Hasher},
        sync::{Arc, atomic::AtomicU32},
        time::Duration,
    };

    use mirrord_config::feature::network::filter::AddressFilter;
    use mirrord_protocol::tcp::{Filter, HttpFilter};
    use rstest::rstest;
    use serde_json::json;
    use uuid::Uuid;

    use crate::session_monitor::chaos::rules::{
        ChaosEffectConnectionError, ChaosEffectLatency, ChaosEffectRequest, ChaosRule,
        ChaosRuleRequest, ChaosSelector, ChaosSelectorRequest, ConnectionErrorType, FsChaosEffect,
        HttpChaosEffect, Percentage, TcpChaosEffect,
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
    #[should_panic(expected = "Unimplemented(\"FS selector\")")]
    #[case::fs_latency(json!({
      "name": "file-connect-slow",
      "effect": {
        "latency": {
          "delay_ms": 250,
        }
      },
      "selector": {
        "file_path": ".+\\.json"
      }
    }), ChaosRuleRequest {
        name: Some("file-connect-slow".to_owned()),
        priority: None,
        effect: ChaosEffectRequest::Latency {
            delay_ms: 250,
            jitter_ms: None
        },
        selector: ChaosSelectorRequest {
            file_path: Some(".+\\.json".to_owned()),
            ..Default::default()
        }
    }, ChaosRule {
        id: Uuid::default(),
        name: Some("file-connect-slow".to_owned()),
        selector: ChaosSelector::Fs {
            file_path: vec![".+\\.json".to_owned()],
            percentage: Percentage::from(100),
            effect: FsChaosEffect::Latency(ChaosEffectLatency {
                delay: Duration::from_millis(250),
                jitter: Duration::default(),
            }),
        },
        ..Default::default()
    })]
    #[should_panic(expected = "Unimplemented(\"HTTP selector\")")]
    #[case::http_latency(json!({
      "name": "http-connect-slow",
      "effect": {
        "latency": {
          "delay_ms": 200,
          "jitter_ms": 50,
        }
      },
      "selector": {
        "upstream": "jadwiga-wawel.pl",
        "path_filter": "^/api/"
      }
    }), ChaosRuleRequest {
        name: Some("http-connect-slow".to_owned()),
        priority: None,
        effect: ChaosEffectRequest::Latency {
            delay_ms: 200,
            jitter_ms: Some(50)
        },
        selector: ChaosSelectorRequest {
            upstream: Some("jadwiga-wawel.pl".to_owned()),
            path_filter: Some("^/api/".to_owned()),
            ..Default::default()
        }
    }, ChaosRule {
        id: Uuid::default(),
        name: Some("http-connect-slow".to_owned()),
        selector: ChaosSelector::Http {
            upstream: AddressFilter::Name("jadwiga-wawel.pl".to_owned(), 0),
            filter: Some(HttpFilter::Path(Filter::new("^/api/".to_owned()).unwrap())),
            percentage: Percentage::from(100),
            effect: HttpChaosEffect::Latency(ChaosEffectLatency {
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
            "type": "timed_out",
            "after_ms": 750
          }
        }
    }), ChaosRuleRequest {
        name: None,
        priority: Some(100),
        effect: ChaosEffectRequest::ConnectionError {
            error_type: "timed_out".to_owned(),
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
            effect: TcpChaosEffect::ConnectionError(ChaosEffectConnectionError {
                error_type: ConnectionErrorType::TimedOut,
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
            file_path: Some("/mnt/data/*.json".to_owned()),
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
    // Reinstate when required protocol implemented
    // #[case::http_effect_with_tcp_selector(json!({
    //     "selector": {
    //       "percentage": 75,
    //       "upstream": "rust-lang.org"
    //     },
    //     "effect": {
    //       "http_override": {
    //           "status_code": 503,
    //       }
    //     }
    // }), ChaosRuleRequest {
    //     name: None,
    //     priority: None,
    //     effect: ChaosEffectRequest::HttpOverride,
    //     selector: ChaosSelectorRequest {
    //         upstream: Some("rust-lang.org".to_owned()),
    //         percentage: Some(75),
    //         ..Default::default()
    //     }
    // })]
    #[case::invalid_selector_upstream_address_filter(json!({
        "selector": {
          "upstream": "meow://i-guess-i-could-be-blaze",
          "percentage": 75
        },
        "priority": 100,
        "effect": {
          "connection_error": {
            "type": "timed_out",
            "after_ms": 750
          }
        }
    }), ChaosRuleRequest {
        name: None,
        priority: Some(100),
        effect: ChaosEffectRequest::ConnectionError {
            error_type: "timed_out".to_owned(),
            after_ms: Some(750)
        },
        selector: ChaosSelectorRequest {
            upstream: Some("meow://i-guess-i-could-be-blaze".to_owned()),
            percentage: Some(75),
            ..Default::default()
        }
    })]
    #[should_panic(expected = "intended panic")]
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

        let rule = ChaosRule::try_from(parsed_request).expect("intended panic");
        println!("{rule:?}");
    }

    #[rstest]
    #[should_panic(expected = "missing field")]
    #[case::missing_selector(json!({
      "name" : "the-crocodile-is-called-vector-apparently",
      "effect": {
        "latency": {
          "delay_ms": 200,
          "jitter_ms": 50,
        }
      }
    }))]
    #[should_panic]
    #[case::multiple_effects(json!({
        "selector": {
          "percentage": 75,
          "upstream": "rust-lang.org"
        },
        "effect": {
          "connection_error": {
            "type": "timed_out",
            "after_ms": 750
          },
          "latency": {
            "delay_ms": 200,
          }
        }
    }))]
    #[should_panic(expected = "invalid type")]
    #[case::non_object_effect(json!({
        "selector": {
          "percentage": 75,
          "upstream": "https://sonic.fandom.com/wiki/Team_Chaotix"
        },
        "effect": {
          "latency": "vector-isn't-a-very-reptilian-name"
        }
    }))]
    fn error_on_parse_malformed_request(#[case] invalid_rule_req: serde_json::Value) {
        let _: ChaosRuleRequest = serde_json::from_str(&invalid_rule_req.to_string()).unwrap();
    }

    /// We have to guarantee that the custom `PartialEq` and the `Hash` impls for [`ChaosRule`] hold
    /// the property of `k1 == k2 -> hash(k1) == hash(k2)`.
    #[test]
    fn partial_eq_and_hash_return_equal() {
        fn hash(rule: &ChaosRule) -> u64 {
            let mut hasher = DefaultHasher::new();
            rule.hash(&mut hasher);
            hasher.finish()
        }

        let rule = ChaosRule {
            id: Uuid::new_v4(),
            name: Some("Zamek w Bobrownikach".to_owned()),
            priority: 10,
            selector: ChaosSelector::Tcp {
                upstream: AddressFilter::Name("zamki.pl".to_owned(), 443),
                percentage: Percentage::from(25),
                effect: TcpChaosEffect::Latency(ChaosEffectLatency {
                    delay: Duration::from_millis(100),
                    jitter: Duration::from_millis(50),
                }),
            },
            hit_count: Arc::new(AtomicU32::new(1377)),
        };
        let same_rule_different_hit_count = ChaosRule {
            hit_count: Arc::new(AtomicU32::new(1405)),
            ..rule.clone()
        };

        assert_eq!(rule, same_rule_different_hit_count);
        assert_eq!(hash(&rule), hash(&same_rule_different_hit_count));
    }

    #[rstest]
    #[case::never(0)]
    #[case::occasionaly(25)]
    #[case::sometimes(42)]
    #[case::sixseven(67)]
    #[case::always(100)]
    fn roll_for_hit_roughly_respects_percentage(#[case] chance: u32) {
        let percentage = Percentage::from(chance);
        let attempts = 100_000;
        let hits = (0..attempts).filter(|_| percentage.roll_for_hit()).count() as f32;
        let actual = hits / attempts as f32;
        let expected = percentage.as_decimal();

        if chance == 100 || chance == 0 {
            assert!(
                (actual - expected).abs() <= 0.,
                "expected roughly {expected:.2}, got {actual:.2}"
            );
        } else {
            assert!(
                (actual - expected).abs() <= 0.02,
                "expected roughly {expected:.2}, got {actual:.2}"
            );
        }
    }
}
