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

pub mod api;
pub mod rules;

use rules::*;

use crate::proxies::{incoming::IncomingProxyMessage, outgoing::OutgoingProxyMessage};

pub type ChaosRuleList = HashSet<ChaosRule>;

pub trait ApplyChaosRuleLol {
    type WhatToDo;

    fn chaos_effect(&self, rules: &ChaosRuleList) -> Option<Self::WhatToDo>;
}

#[derive(Debug, Clone)]
pub struct ChaosWatcherRx(watch::Receiver<ChaosRuleList>);

impl ChaosWatcherRx {
    pub fn new(rx: watch::Receiver<ChaosRuleList>) -> Self {
        Self(rx)
    }

    pub fn chaos_effect<'a, M>(&'a self, message: &'a M) -> Option<M::WhatToDo>
    where
        M: ApplyChaosRuleLol + ?Sized,
    {
        let rules = self.0.borrow();
        message.chaos_effect(&rules)
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
