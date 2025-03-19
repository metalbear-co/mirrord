use std::{collections::HashMap, fmt};

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Custom cluster-wide resource for storing a reusable mirrord config template.
///
/// Can be selected from the user's mirrord config.
#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    // The operator group is handled by the operator, we want profiles to be handled by k8s.
    group = "profiles.mirrord.metalbear.co",
    version = "v1alpha",
    kind = "MirrordProfile"
)]
#[serde(rename_all = "camelCase")]
pub struct MirrordProfileSpec {
    /// A list of adjustments to be made in the user's feature config.
    ///
    /// The adjustments are applied in order.
    pub feature_adjustments: Vec<FeatureAdjustment>,

    /// For future compatibility.
    ///
    /// The CLI should error when the profile contains unknown fields.
    #[schemars(skip)]
    #[serde(flatten, skip_serializing)]
    pub unknown_fields: HashMap<String, Value>,
}

/// A single adjustment to the mirrord config's `feature` section.
#[derive(Deserialize, Serialize, JsonSchema, Debug, Clone)]
pub struct FeatureAdjustment {
    /// The kind of feature to which this adjustment applies.
    pub kind: FeatureKind,
    /// The change to be made.
    pub change: FeatureChange,

    /// For future compatibility.
    ///
    /// The CLI should error when the profile contains unknown fields.
    #[schemars(skip)]
    #[serde(flatten, skip_serializing)]
    pub unknown_fields: HashMap<String, Value>,
}

/// A kind of mirrord feature that can be adjusted in a mirrord policy.
#[derive(Deserialize, Serialize, JsonSchema, Debug, Clone, Copy)]
#[serde(rename_all = "camelCase")]
pub enum FeatureKind {
    /// Incoming traffic.
    Incoming,
    /// Outgoing traffic.
    Outgoing,
    /// DNS resolution.
    Dns,
    /// For forward compatibility.
    ///
    /// We don't want a single deserialization failure to fail listing policies.
    #[schemars(skip)]
    #[serde(other)]
    Unknown,
}

impl fmt::Display for FeatureKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let as_str = match self {
            Self::Incoming => "incoming",
            Self::Outgoing => "outgoing",
            Self::Dns => "dns",
            Self::Unknown => "unknown",
        };

        f.write_str(as_str)
    }
}

/// An adjustment to some mirrord feature.
#[derive(Deserialize, Serialize, JsonSchema, Debug, Clone, Copy)]
#[serde(rename_all = "camelCase")]
pub enum FeatureChange {
    /// Incoming traffic will be mirrored.
    ///
    /// Applies only to the `incoming` feature kind.
    Mirror,
    /// Incoming traffic will be stolen.
    ///
    /// Applies only to the `incoming` feature kind.
    Steal,
    /// Disables the feature.
    ///
    /// When applied to:
    /// * `incoming` - no remote traffic will be intercepted
    /// * `outgoing` - all outgoing traffic will be fully local
    /// * `dns` - all DNS resolution will be fully local
    Disabled,
    /// Outgoing traffic or DNS resolution will be fully remote.
    ///
    /// Applies only to the `outgoing` and `dns` feature kinds.
    Remote,
    /// For forward compatibility.
    ///
    /// We don't want a single deserialization failure to fail listing policies.
    #[schemars(skip)]
    #[serde(other)]
    Unknown,
}

impl fmt::Display for FeatureChange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let as_str = match self {
            Self::Mirror => "mirror",
            Self::Steal => "steal",
            Self::Disabled => "disabled",
            Self::Remote => "remote",
            Self::Unknown => "unknown",
        };

        f.write_str(as_str)
    }
}
