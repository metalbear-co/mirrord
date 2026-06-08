use std::{
    borrow::Borrow,
    hash::Hash,
    sync::{Arc, atomic::AtomicU32},
    time::Duration,
};

use mirrord_config::feature::network::filter::AddressFilter;
use mirrord_protocol::tcp::HttpFilter;
use strum_macros::EnumString;
use uuid::Uuid;

// having separate enums per selector allows invalid effect/ selector combos to
// be impossible
#[derive(Default, Debug, PartialEq, Hash)]
pub enum TcpChaosEffect {
    Latency(ChaosEffectLatency),
    ConnectionError(ChaosEffectConnError),
    Degradation,
    #[default]
    Nothing,
}

#[derive(Default, Debug, PartialEq, Hash)]
pub enum HttpChaosEffect {
    Latency(ChaosEffectLatency),
    HttpOverride,
    #[default]
    Nothing,
}

#[derive(Default, Debug, PartialEq, Hash)]
pub enum FsChaosEffect {
    Latency(ChaosEffectLatency),
    FsError,
    #[default]
    Nothing,
}

#[derive(Debug, PartialEq, Hash)]
pub struct ChaosEffectLatency {
    pub delay: Duration, // req
    pub jitter: Duration,
}

#[derive(Debug, PartialEq, Hash)]
pub struct ChaosEffectConnError {
    pub error_type: ConnErrorType, // req
    pub after: Duration,
}

#[derive(Debug, PartialEq, EnumString, Hash)]
#[strum(ascii_case_insensitive)]
pub enum ConnErrorType {
    Reset,   // TCP RST
    Timeout, // hangs then closes
    Refused, // ECONNREFUSED
}

/// Helper type for a number between 0 and 100 inclusive. Defaults to 100%. Values larger than 100%
/// get rounded down to 100%.
#[derive(Clone, Copy, Debug, PartialEq, Hash)]
pub struct Percentage(u32);

impl Percentage {
    pub fn new(value: u32) -> Self {
        Self(value.min(100))
    }
}

#[derive(Default, Debug, PartialEq, Hash)]
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
    pub hit_count: Arc<AtomicU32>,
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
