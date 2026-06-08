use std::str::FromStr;

use anyhow::{Context, anyhow};
use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;

use crate::session_monitor::chaos::*;

/// Represents a rule request from POST requests, corresponding to a rule that is not yet validated.
/// In converting [`Self`](ChaosRuleRequest) to a [`ChaosRule`], the rule becomes validated.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[skip_serializing_none]
pub(super) struct ChaosRuleRequest {
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
    percentage: Option<u32>,
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
        let percentage = selector.percentage.map(Percentage::new).unwrap_or_default();

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
