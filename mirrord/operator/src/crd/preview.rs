//! CRD types for preview environments.
//!
//! The CLI creates a [`PreviewSession`] resource in the cluster, and the operator reconciles
//! it by spawning a preview pod and routing traffic to it. The CR's status subresource
//! tracks the session lifecycle (`Initializing` → `Waiting` → `Ready` / `Failed`), which
//! the CLI watches to report progress back to the user.

use k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime;
use kube::CustomResource;
use mirrord_config::{
    config::ConfigError,
    feature::network::incoming::{IncomingConfig, IncomingMode},
    target::Target,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::session::SessionTarget;

/// This resource represents a preview environment created by the `mirrord preview start` command.
#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[kube(
    group = "preview.mirrord.metalbear.co",
    version = "v1alpha",
    kind = "PreviewSession",
    root = "PreviewSession",
    status = "PreviewSessionStatus",
    namespaced
)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSessionSpec {
    /// User's container image to run in the preview pod.
    pub image: String,

    /// Environment key used to group related preview pods and for traffic filtering.
    pub key: String,

    /// Target to copy pod configuration from (deployment, pod, statefulset, etc.).
    /// The preview pod will be a copy of the target's pod spec with the user's image.
    pub target: SessionTarget,

    /// Incoming traffic configuration for the preview environment.
    ///
    /// Specifies which ports to steal/mirror traffic from and optional HTTP filters.
    /// This configuration is extracted from the user's mirrord config.
    /// When `None`, incoming traffic is not intercepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incoming: Option<PreviewIncomingConfig>,

    /// How long (in seconds) this session is allowed to live.
    /// The operator will terminate the session when this time elapses.
    pub ttl_secs: u64,
}

impl PreviewSessionSpec {
    /// Convert the [`SessionTarget`] into a [`mirrord_config::target::Target`].
    pub fn parsed_target(&self) -> Result<Target, ConfigError> {
        self.target.as_target()
    }
}

/// Status of a preview session resource.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSessionStatus {
    /// Current lifecycle phase of the session.
    pub phase: PreviewSessionPhase,

    /// Timestamp of when the operator started processing this session.
    pub started_at: MicroTime,

    /// Name of the preview pod created for this session.
    ///
    /// Set once the pod is created (from the `Waiting` phase onward). `None` during
    /// `Initializing` or if pod creation failed before a name was assigned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pod_name: Option<String>,

    /// Human-readable description of why the session failed.
    ///
    /// Only set when `phase` is `Failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_message: Option<String>,
}

/// Phase of a preview session's lifecycle.
///
/// Progresses through `Initializing` → `Waiting` → `Ready`. Any phase may transition to
/// `Failed` on error.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, Eq, PartialEq)]
pub enum PreviewSessionPhase {
    /// Operator is setting up — the preview pod has not been created yet.
    Initializing,
    /// Preview pod has been created, waiting for it to become ready.
    Waiting,
    /// Preview pod is running and traffic routing is active.
    Ready,
    /// Session has encountered an unrecoverable error.
    Failed,
}

/// Incoming traffic configuration for preview environments.
///
/// Extracted from the user's mirrord config by the CLI and included in the CR.
/// The operator uses this configuration to set up traffic stealing/mirroring from
/// the original target to the preview pod.
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewIncomingConfig {
    /// Explicit list of ports to steal/mirror. When `None`, the operator discovers ports from
    /// the preview pod's container port declarations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ports: Option<Vec<u16>>,

    /// Whether to steal (`true`) or mirror (`false`) traffic from the target.
    pub steal: bool,

    /// JSON-serialized `HttpFilterConfig`. Stored as an opaque string to avoid coupling
    /// the CRD schema to `HttpFilterConfig`, which is too large to manually duplicate here
    /// and could break backwards compatibility if stored directly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_filter: Option<String>,
}

impl PreviewIncomingConfig {
    /// Converts from the user's incoming config. Returns `None` when the mode is `Off`.
    pub fn from_config(value: &IncomingConfig) -> Option<Self> {
        match value.mode {
            IncomingMode::Off => None,
            IncomingMode::Mirror | IncomingMode::Steal => Some(Self {
                ports: value.ports.as_ref().map(|p| p.iter().copied().collect()),
                steal: matches!(value.mode, IncomingMode::Steal),
                http_filter: value
                    .http_filter
                    .is_filter_set()
                    .then(|| serde_json::to_string(&value.http_filter))
                    .transpose()
                    .expect("HttpFilterConfig serialization cannot fail"),
            }),
        }
    }
}
