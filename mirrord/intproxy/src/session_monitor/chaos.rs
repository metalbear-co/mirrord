//! Here we have some helper types and wrappers for dealing with the mirrord chaos feature, and the
//! [`chaos_router`].
//!
//! Instead of just using the [`watch`] channels directly, we wrap them in [`ChaosWatcherRx`] and
//! [`ChaosWatcherTx`] to provide a more specific api to the chaos stuff.
//!
//! We also have the [`SessionId`] helper type here, because our `session_id`s can be of different
//! types when using the operator, or not.
use std::{
    borrow::Borrow,
    collections::HashSet,
    hash::Hash,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
};

use axum::{
    Router,
    routing::{post, put},
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tokio::sync::watch;
use tracing::Level;
use uuid::Uuid;

pub mod analytics;
pub mod api;
pub mod rules;

use rules::*;

use crate::session_monitor::{api::AppState, chaos::api::*};

/// Routes for `/api/chaos/rules`.
///
/// - `POST /`: creates a new rule **unconditionally** for the session;
/// - `DELETE /`: deletes every rule of the session;
/// - `GET /`: gets the list of rules for the session;
/// - `PUT /{rule_id}`: updates the rule for the session;
/// - `DELETE /{rule_id}`: deletes the rule of the session;
/// - `GET /{rule_id}`: gets the rule of the session.
pub(super) fn chaos_router() -> Router<AppState> {
    Router::new()
        .route(
            "/",
            post(post_create_rule)
                .delete(delete_clear_session_rules)
                .get(get_list_active_rules_for_session),
        )
        .route(
            "/{rule_id}",
            put(put_update_rule).delete(delete_rule).get(get_rule),
        )
}

pub type ChaosRuleList = HashSet<ChaosRule>;

/// mirrord sessions can be either a [`Uuid`] when `operator = false`, or this random string when
/// the operator is responsible for creating a session. To typefy our REST apis, we have this thing.
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
/// [`chaos_router`].
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

    /// Writes the `new_rule` into the [`ChaosRuleList`] and returns the [`ChaosRule`] if we
    /// actually inserted something (the rule did not exist already).
    #[tracing::instrument(level = Level::DEBUG)]
    pub(super) fn create_rule(&self, new_rule: ChaosRule) -> Option<ChaosRule> {
        let mut created = false;

        self.0
            .send_modify(|current_rules| created = current_rules.insert(new_rule.clone()));

        created.then_some(new_rule)
    }

    /// Returns the list of [`ChaosRule`] as a `Vec`. Don't use this if you're trying to iterate on
    /// the rules to check for something, this is only really useful for returning things through
    /// the chaos api.
    #[tracing::instrument(level = Level::DEBUG)]
    pub(super) fn list_active_rules_for_session(&self) -> Vec<ChaosRule> {
        self.0.borrow().iter().cloned().collect()
    }

    /// Deletes every [`ChaosRule`] in the storage, by replacing the whole thing with an empty
    /// `HashSet`.
    #[tracing::instrument(level = Level::DEBUG)]
    pub(super) fn clear_session_rules(&self) {
        self.0.send_replace(Default::default());
    }

    /// Updates a [`ChaosRule`] whose [`ChaosRule::id`] matches `new_rule`'s.
    ///
    /// This is possible because of the custom `PartialEq` and `Hash` implementations of
    /// `ChaosRule`.
    #[tracing::instrument(level = Level::DEBUG)]
    pub(super) fn update_rule(&self, new_rule: ChaosRule) -> Option<ChaosRule> {
        let mut old_rule = None;
        self.0
            .send_modify(|current_rules| old_rule = current_rules.replace(new_rule));

        old_rule
    }

    /// Deletes a [`ChaosRule`] from the `HashSet`, returning it if it was present.
    #[tracing::instrument(level = Level::DEBUG, ret)]
    pub(super) fn delete_rule(&self, rule_id: Uuid) -> Option<ChaosRule> {
        let mut deleted_rule = None;
        self.0.send_modify(|current_rules| {
            deleted_rule = current_rules.take(&rule_id);
        });

        deleted_rule
    }

    /// Returns a cloned [`ChaosRule`] from the `HashSet`.
    #[tracing::instrument(level = Level::DEBUG)]
    fn get_rule(&self, rule_id: Uuid) -> Option<ChaosRule> {
        self.0.borrow().get(&rule_id).cloned()
    }
}

/// Implements `serialize` and `deserialize` for the [`ChaosRule::hit_count`] [`Arc<AtomicU32>`].
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
