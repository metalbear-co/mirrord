use std::{
    borrow::Borrow,
    collections::HashSet,
    hash::Hash,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tokio::sync::watch;
use tracing::Level;
use uuid::Uuid;

pub mod analytics;
pub mod api;
pub mod rules;

use rules::*;

pub type ChaosRuleList = HashSet<ChaosRule>;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SessionId {
    Uuid(Uuid),
    VarChar(String),
}

impl core::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionId::VarChar(s) => f.write_fmt(format_args!("{s}")),
            SessionId::Uuid(uuid) => f.write_fmt(format_args!("{uuid}")),
        }
    }
}

/// Wrapper for the [`watch::Receiver`] that holds the state of the [`ChaosRule`]s for an instance
/// of intproxy.
///
/// The sender side is held by the [`super::api::AppState`], and this state can be changed by the
/// [`api::chaos_router`].
#[derive(Debug, Clone)]
pub struct ChaosWatcherRx(watch::Receiver<ChaosRuleList>);

impl ChaosWatcherRx {
    pub fn new(rx: watch::Receiver<ChaosRuleList>) -> Self {
        Self(rx)
    }

    /// Does not mark the value as "seen", it calls [`watch::Receiver::borrow`] .
    pub(crate) fn borrow(&self) -> watch::Ref<'_, ChaosRuleList> {
        self.0.borrow()
    }
}

/// Wrapper for the [`watch::Sender`] that holds the state of the [`ChaosRule`]s for an instance
/// of intproxy.
///
/// The receivers are held by the background tasks of the intproxy, e.g.`OutgoingProxy`.
#[derive(Debug, Clone)]
pub struct ChaosWatcherTx(watch::Sender<ChaosRuleList>);

impl ChaosWatcherTx {
    pub fn new(tx: watch::Sender<ChaosRuleList>) -> Self {
        Self(tx)
    }

    #[tracing::instrument(level = Level::INFO)]
    pub(super) fn create_rule(&self, new_rule: ChaosRule) -> Option<ChaosRule> {
        let mut created = false;

        self.0
            .send_modify(|current_rules| created = current_rules.insert(new_rule.clone()));

        created.then_some(new_rule)
    }

    #[tracing::instrument(level = Level::INFO)]
    pub(super) fn list_active_rules_for_session(&self) -> Vec<ChaosRule> {
        self.0.borrow().iter().cloned().collect()
    }

    #[tracing::instrument(level = Level::INFO)]
    pub(super) fn clear_session_rules(&self) {
        self.0.send_replace(Default::default());
    }

    #[tracing::instrument(level = Level::INFO)]
    pub(super) fn update_rule(&self, new_rule: ChaosRule) -> Option<ChaosRule> {
        let mut old_rule = None;
        self.0
            .send_modify(|current_rules| old_rule = current_rules.replace(new_rule));

        old_rule
    }

    #[tracing::instrument(level = Level::INFO, ret)]
    pub(super) fn delete_rule(&self, rule_id: Uuid) -> Option<ChaosRule> {
        let mut deleted_rule = None;
        self.0.send_modify(|current_rules| {
            deleted_rule = current_rules.take(&rule_id);
        });

        deleted_rule
    }

    #[tracing::instrument(level = Level::INFO)]
    fn get_rule(&self, rule_id: Uuid) -> Option<ChaosRule> {
        self.0.borrow().get(&rule_id).cloned()
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
