use std::collections::HashMap;

use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Custom cluster-wide resource for storing a reusable mirrord config template.
///
/// Can be selected from the user's mirrord config.
///
/// Mind that mirrord profiles are only a functional feature.
/// mirrord Operator is not able to enforce that the
/// application running on the user's machine follows the selected profile.
///
/// This feature should not be used in order to prevent malicious actions.
#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "profiles.mirrord.metalbear.co",
    version = "v1alpha",
    kind = "MirrordClusterProfile",
    category = "mirrord",
    status = "ProfileStatus",
    printcolumn = r#"{"name":"Accepted", "type":"string", "description":"Whether the CLI can apply this profile.", "jsonPath":".status.conditions[?(@.type==\"Accepted\")].status"}"#,
    printcolumn = r#"{"name":"Reason", "type":"string", "description":"Why the profile cannot be applied.", "jsonPath":".status.conditions[?(@.type==\"Accepted\")].reason"}"#,
    printcolumn = r#"{"name":"Detail", "type":"string", "description":"Which part of the profile the CLI rejects.", "jsonPath":".status.conditions[?(@.type==\"Accepted\")].message", "priority":1}"#,
    printcolumn = r#"{"name":"Age", "type":"date", "description":"Time since the resource was created.", "jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct MirrordClusterProfileSpec {
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

/// Custom namespaced resource for storing a reusable mirrord config template.
///
/// Can be selected from the user's mirrord config.
///
/// Example:
/// ```json
/// {
///     "profile": "my-namespace/my-profile"
/// }
/// ```
#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    // The operator group is handled by the operator, we want profiles to be handled by k8s.
    group = "profiles.mirrord.metalbear.co",
    version = "v1alpha",
    kind = "MirrordProfile",
    category = "mirrord",
    namespaced,
    status = "ProfileStatus",
    printcolumn = r#"{"name":"Accepted", "type":"string", "description":"Whether the CLI can apply this profile.", "jsonPath":".status.conditions[?(@.type==\"Accepted\")].status"}"#,
    printcolumn = r#"{"name":"Reason", "type":"string", "description":"Why the profile cannot be applied.", "jsonPath":".status.conditions[?(@.type==\"Accepted\")].reason"}"#,
    printcolumn = r#"{"name":"Detail", "type":"string", "description":"Which part of the profile the CLI rejects.", "jsonPath":".status.conditions[?(@.type==\"Accepted\")].message", "priority":1}"#,
    printcolumn = r#"{"name":"Age", "type":"date", "description":"Time since the resource was created.", "jsonPath":".metadata.creationTimestamp"}"#
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
#[serde(rename_all = "camelCase")]
pub struct FeatureAdjustment {
    /// The change to be made in the user's config.
    pub change: FeatureChange,

    /// For future compatibility.
    ///
    /// The CLI should error when the profile contains unknown fields.
    #[schemars(skip)]
    #[serde(flatten, skip_serializing)]
    pub unknown_fields: HashMap<String, Value>,
}

/// Whether the operator has observed a profile and judged it usable.
///
/// A profile is applied by the *CLI*, not the operator, and the CLI hard-errors on an unknown field
/// or an unrecognised change. Until this status existed the author of a profile learned that only
/// when somebody selected it and their session failed.
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProfileStatus {
    /// `metadata.generation` of the spec the operator last reconciled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(extend("x-kubernetes-list-type" = "map", "x-kubernetes-list-map-keys" = ["type"]))]
    pub conditions: Vec<Condition>,
}

/// An adjustment to some mirrord feature.
#[derive(Deserialize, Serialize, JsonSchema, Debug, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
pub enum FeatureChange {
    /// Incoming traffic will be mirrored.
    IncomingMirror,
    /// Incoming traffic will be stolen.
    IncomingSteal,
    /// Incoming traffic will not be intercepted
    IncomingOff,
    /// All DNS resolution will be remote.
    DnsRemote,
    /// All DNS resolution will be local.
    DnsOff,
    /// All outgoing traffic will be remote.
    OutgoingRemote,
    /// All outgoing traffic will be local.
    OutgoingOff,
    /// For forward compatibility.
    ///
    /// We don't want a single deserialization failure to fail listing policies.
    #[schemars(skip)]
    #[serde(other)]
    Unknown,
}
