//! The wire protocol of `mirrord preview wake`, which wakes a preview environment and keeps it
//! from idling while the CLI waits for it.
//!
//! The CLI opens the `previews/{name}/wake` websocket on the operator, and the operator answers
//! with a [`WakeStatus`] as a JSON text message once it observed the preview on every one of its
//! clusters, and then whenever the preview's state on one of them changes, so that a CLI that
//! connects again learns nothing it didn't know but what changed. A state that the CLI acts on
//! whatever the rest is, such as a failure, goes out at once, with each cluster not observed yet
//! as [`WakeState::Pending`]. The CLI sends nothing: the operator holds the preview awake for as
//! long as the connection lasts, and the idle countdown starts over once it closes.
//!
//! A multi-cluster primary opens the same websocket on the operator of each workload cluster that
//! runs a copy of the preview, and relays what each one reports under that cluster's name.
//!
//! The messages are JSON rather than `bincode`, so that each enum can carry an `Unknown` variant
//! and a CLI keeps reading the states that a newer operator adds.
//!
//! This module needs no feature of the crate.

use std::ops::Not;

use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests;

/// Operator to CLI: the state of the preview on every cluster that the operator wakes it on.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WakeStatus {
    pub clusters: Vec<ClusterWake>,
}

/// The state of the preview on one cluster.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ClusterWake {
    /// The cluster, as a multi-cluster primary names it. `None` for the cluster of an operator
    /// that is no multi-cluster primary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster: Option<String>,

    /// Whether the cluster runs a replica copy of the preview, rather than the preview itself.
    /// The CLI only warns about a replica that can't be woken, and waits for the rest.
    #[serde(default, skip_serializing_if = "Not::not")]
    pub replica: bool,

    pub state: WakeState,
}

/// How the wake of the preview on one cluster gets on.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum WakeState {
    /// The operator hasn't asked the preview to wake yet: no task of the preview's runs once the
    /// operator waited for one to register, or the cluster wasn't observed yet when another
    /// cluster's state went out at once.
    Pending,

    /// The operator asked the preview to wake, and its pods aren't ready yet.
    Waking,

    /// The preview's pods are ready.
    Ready,

    /// A `mirrord exec` session replaces the preview, so nothing runs it until that session ends.
    Paused,

    /// The preview failed, or is being deleted, so it won't be ready.
    Failed { message: String },

    /// The operator couldn't reach the operator of the cluster, and keeps trying.
    Unreachable { message: String },

    /// The operator lost its connection to the operator of the cluster, which held the preview,
    /// and connects again: the cluster's state is unknown until the new connection reports it,
    /// or turns [`Self::Unreachable`] when it can't be made.
    Reconnecting { message: String },

    /// The operator of the cluster is connected but hasn't reported the copy yet, for longer
    /// than it usually takes, as `message` says. Its first report replaces this, and the cluster
    /// turns [`Self::Unreachable`] when none comes for much longer.
    NotReported { message: String },

    /// The operator of the cluster, which runs a replica copy of the preview, can't wake
    /// previews.
    Unsupported,

    /// A state that an operator newer than the CLI sends.
    #[serde(other)]
    Unknown,
}
