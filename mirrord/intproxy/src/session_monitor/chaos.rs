use std::{
    borrow::Borrow,
    collections::HashSet,
    hash::Hash,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};

use mirrord_config::feature::network::filter::AddressFilter;
use mirrord_intproxy_protocol::OutgoingConnectRequest;
use mirrord_protocol::tcp::HttpFilter;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use strum_macros::EnumString;
use thiserror::Error;
use tokio::sync::watch;
use uuid::Uuid;

pub mod api;

pub type ChaosRuleList = HashSet<ChaosRule>;

#[derive(Debug, Clone)]
pub struct ChaosWatcherRx(watch::Receiver<ChaosRuleList>);

impl ChaosWatcherRx {
    pub fn new(rx: watch::Receiver<ChaosRuleList>) -> Self {
        Self(rx)
    }

    pub(crate) fn chaos_effect(
        &self,
        OutgoingConnectRequest {
            remote_address,
            protocol,
        }: &OutgoingConnectRequest,
    ) -> Option<ChaosSelector> {
        let rules = self.0.borrow();
        rules.iter().find_map(|rule| match &rule.selector {
            ChaosSelector::Tcp {
                upstream,
                percentage,
                effect,
            } => Some(rule.selector.clone()),
            ChaosSelector::Http {
                upstream,
                percentage,
                filter,
                effect,
            } => todo!(),
            ChaosSelector::Fs {
                file_path,
                percentage,
                effect,
            } => todo!(),
            ChaosSelector::None => todo!(),
        })
    }
}

#[derive(Debug, Clone)]
pub struct ChaosWatcherTx(watch::Sender<ChaosRuleList>);

impl ChaosWatcherTx {
    pub fn new(tx: watch::Sender<ChaosRuleList>) -> Self {
        Self(tx)
    }

    pub(super) fn create_rule(&self, new_rule: ChaosRule) {
        self.0.send_modify(|current_rules| {
            current_rules.insert(new_rule);
        });
    }

    pub(super) fn list_active_rules_for_session(&self) -> ChaosRuleList {
        self.0.borrow().clone()
    }

    pub(super) fn clear_session_rules(&self) {
        self.0.send_replace(Default::default());
    }

    pub(super) fn update_rule(&self, new_rule: ChaosRule) {
        self.0.send_modify(|current_rules| {
            current_rules.replace(new_rule);
        });
    }

    pub(super) fn delete_rule(&self, rule_id: Uuid) {
        self.0.send_modify(|current_rules| {
            current_rules.remove(&rule_id);
        });
    }

    fn get_rule(&self, rule_id: Uuid) -> Option<ChaosRule> {
        self.0.borrow().get(&rule_id).cloned()
    }
}
#[derive(Debug, Error)]
pub enum ChaosRuleError {
    #[error("the chaos rule request was invalid: {0}")]
    Invalid(anyhow::Error),

    #[error("this feature is not yet available in mirrord chaos: {0}")]
    Unimplemented(String),
}

// having separate enums per selector allows invalid effect/ selector combos to
// be impossible
#[derive(Clone, Default, Debug, PartialEq, Hash, Serialize, Deserialize)]
pub enum TcpChaosEffect {
    Latency(ChaosEffectLatency),
    ConnectionError(ChaosEffectConnError),
    Degradation,
    #[default]
    Nothing,
}

#[derive(Clone, Serialize, Deserialize, Default, Debug, PartialEq, Hash)]
pub enum HttpChaosEffect {
    Latency(ChaosEffectLatency),
    HttpOverride,
    #[default]
    Nothing,
}

#[derive(Clone, Serialize, Deserialize, Default, Debug, PartialEq, Hash)]
pub enum FsChaosEffect {
    Latency(ChaosEffectLatency),
    FsError,
    #[default]
    Nothing,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Hash)]
pub struct ChaosEffectLatency {
    pub delay: Duration, // req
    pub jitter: Duration,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Hash)]
pub struct ChaosEffectConnError {
    pub error_type: ConnErrorType, // req
    pub after: Duration,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, EnumString, Hash)]
#[strum(ascii_case_insensitive)]
pub enum ConnErrorType {
    Reset,   // TCP RST
    Timeout, // hangs then closes
    Refused, // ECONNREFUSED
}

/// Helper type for a number between 0 and 100 inclusive. Defaults to 100%. Values larger than 100%
/// get rounded down to 100%.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Hash)]
pub struct Percentage(u32);

impl Percentage {
    pub fn new(value: u32) -> Self {
        Self(value.min(100))
    }
}

impl Default for Percentage {
    fn default() -> Self {
        Self::new(100)
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
    /// applied. Cannot be specified by the user, starts at zero.
    #[serde(with = "atomic_u32_arc")]
    pub hit_count: Arc<AtomicU32>,
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
            name,
            priority,
            selector,
            ..
        }: &Self,
    ) -> bool {
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

mod atomic_u32_arc {
    use super::*;

    pub fn serialize<S>(value: &Arc<AtomicU32>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u32(value.load(Ordering::Relaxed))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Arc<AtomicU32>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = u32::deserialize(deserializer)?;
        Ok(Arc::new(AtomicU32::new(value)))
    }
}
