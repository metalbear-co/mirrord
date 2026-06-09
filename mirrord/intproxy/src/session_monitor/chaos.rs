use std::{
    borrow::Borrow,
    collections::HashSet,
    hash::Hash,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
};

use mirrord_intproxy_protocol::OutgoingConnectRequest;
use serde::{Deserialize, Deserializer, Serializer};
use tokio::sync::watch;
use uuid::Uuid;

use crate::session_monitor::chaos::rules::{ChaosRule, ChaosSelector};

pub mod api;
pub mod rules;

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
