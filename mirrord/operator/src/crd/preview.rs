use std::{collections::HashSet, str::FromStr};

use kube::CustomResource;
use mirrord_config::{
    feature::network::incoming::{IncomingConfig, IncomingMode},
    target::Target,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Simplified incoming traffic configuration for preview environments.
///
/// Extracted from the user's mirrord config by the CLI and included in the CRD.
/// The operator uses this configuration to set up traffic stealing/mirroring from
/// the original target to the preview pod.
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewIncomingConfig {
    /// Ports to steal/mirror traffic from.
    ///
    /// When specified, only traffic on these ports will be redirected to the preview pod.
    /// If not specified, all ports will be considered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ports: Option<HashSet<u16>>,

    #[serde(skip_serializing_if = "IncomingMode::is_off")]
    pub mode: IncomingMode,

    /// HTTP header filter for traffic stealing.
    ///
    /// When specified, only HTTP requests matching this header filter will be redirected
    /// to the preview pod. Format: "Header-Name: value" or "Header-Name: regex-pattern".
    ///
    /// This allows for selective traffic routing based on request headers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_header_filter: Option<String>,
}

impl From<&IncomingConfig> for PreviewIncomingConfig {
    fn from(value: &IncomingConfig) -> Self {
        Self {
            ports: value.ports.clone(),
            mode: value.mode,
            http_header_filter: value.http_filter.header_filter.clone(),
        }
    }
}

/// This resource represents a preview environment created by the `mirrord preview start` command.
#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[kube(
    group = "preview.mirrord.metalbear.co",
    version = "v1alpha",
    kind = "PreviewSession",
    root = "PreviewSessionCrd",
    status = "PreviewSessionStatus",
    namespaced
)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSession {
    /// User's container image to run in the preview pod.
    pub image: String,

    /// Environment key used to group related preview pods and for traffic filtering.
    pub key: String,

    /// Target to copy pod configuration from (deployment, pod, statefulset, etc.).
    /// The preview pod will be a copy of the target's pod spec with the user's image.
    pub target: String,

    /// Target namespace (defaults to the default namespace if not specified).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_namespace: Option<String>,

    /// Incoming traffic configuration for the preview environment.
    ///
    /// Specifies which ports to steal/mirror traffic from and optional HTTP filters.
    /// This configuration is extracted from the user's mirrord config.
    pub incoming: PreviewIncomingConfig,
}

impl PreviewSession {
    /// Parse the target string into a [`Target`].
    pub fn parsed_target(&self) -> Result<Target, mirrord_config::config::ConfigError> {
        Target::from_str(&self.target)
    }
}

/// Status of a preview session resource.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub enum PreviewSessionStatus {
    /// Preview pod hasn't been created yet - operator is still setting things up.
    Initializing {
        // kube-rs does not allow mixing unit variants with tuple/struct variants, so this variant
        // has to be a tuple/struct too. If we leave the tuple empty, k8s complains about an object
        // without any items, and kube-rs does not support internally tagged enums, so we actually
        // have to put something in there, even if we don't actually care about that info.
        start_time_utc: String,
    },
    /// Preview pod has been successfully created, waiting for it to be ready.
    Waiting {
        /// Name of the preview pod.
        pod_name: String,
    },
    /// Preview pod is running and ready.
    Ready {
        /// Name of the preview pod.
        pod_name: String,
    },
    /// Preview creation failed.
    Failed {
        /// Name of the preview pod.
        /// This will be used by the controller's finalizer to delete the preview pod.
        pod_name: Option<String>,
        /// Message describing the reason for failure.
        failure_message: String,
    },
}

impl PreviewSessionStatus {
    pub fn pod_name(&self) -> Option<&String> {
        match self {
            Self::Initializing { .. } => None,
            Self::Waiting { pod_name } | Self::Ready { pod_name } => Some(pod_name),
            Self::Failed { pod_name, .. } => pod_name.as_ref(),
        }
    }
}
