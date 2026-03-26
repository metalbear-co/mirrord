//! CRD types for preview environments.
//!
//! The CLI creates a [`PreviewSession`] resource in the cluster, and the operator reconciles
//! it by spawning a preview pod and routing traffic to it. The CR's status subresource
//! tracks the session lifecycle (`Initializing` → `Waiting` → `Ready` / `Failed`), which
//! the CLI watches to report progress back to the user.

use std::{collections::BTreeMap, time::Duration};

use k8s_openapi::{apimachinery::pkg::apis::meta::v1::MicroTime, jiff::Timestamp};
use kube::{
    Api, Client, CustomResource,
    api::{Patch, PatchParams},
};
use mirrord_config::{
    feature::{
        network::incoming::{IncomingConfig, IncomingMode},
        preview::PreviewTtlMins,
        split_queues::{QueueId, SplitQueuesConfig},
    },
    target::Target,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::session::SessionTarget;
#[cfg(feature = "client")]
use crate::client::connect_params::BranchDbNames;

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

    /// How long (in seconds) this session is allowed to live.
    /// Values >= `u32::MAX` are treated as infinite.
    pub ttl_secs: u64,

    /// Incoming traffic configuration for the preview environment.
    ///
    /// Specifies which ports to steal/mirror traffic from and optional HTTP filters.
    /// This configuration is extracted from the user's mirrord config.
    /// When `None`, incoming traffic is not intercepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incoming: Option<PreviewIncomingConfig>,

    /// Queue splitting configuration for this preview session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_splitting: Option<PreviewQueueSplittingConfig>,

    /// Database branching configuration for this preview session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub db_branching: Option<PreviewDbBranchingConfig>,
}

impl PreviewSessionSpec {
    /// Convert the [`SessionTarget`] into a [`mirrord_config::target::Target`].
    pub fn config_target(&self) -> Option<Target> {
        self.target.clone().into_config()
    }

    /// Returns `true` when `ttl_secs` should be treated as infinite.
    pub fn has_infinite_ttl(&self) -> bool {
        self.ttl_secs >= PreviewTtlMins::INFINITE_TTL_SECS
    }
}

impl PreviewSession {
    pub fn runtime_secs(&self) -> u32 {
        self.status
            .as_ref()
            .and_then(|status| {
                Duration::try_from(Timestamp::now().duration_since(status.started_at.0)).ok()
            })
            .map(|runtime| runtime.as_secs().try_into().unwrap_or(u32::MAX))
            .unwrap_or_default()
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

    /// Timestamp when the session's TTL expires.
    ///
    /// Set when the session enters the `Ready` phase for finite TTL sessions.
    /// Computed as `now + ttl_secs` at the moment the operator starts the TTL countdown.
    /// `None` for infinite TTL, during earlier phases, or when running against an older
    /// operator that does not set this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<MicroTime>,
}

/// Phase of a preview session's lifecycle.
///
/// Progresses through `Initializing` → `Waiting` → `Ready`. Any phase may transition to
/// `Failed` on error.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, Eq, PartialEq)]
pub enum PreviewSessionPhase {
    /// Operator is setting up — the preview pod has not been created yet.
    Initializing,
    /// Preview pod has been created, waiting for it to become ready.
    Waiting,
    /// Preview pod is running and traffic routing is active.
    Ready,
    /// Session has encountered an unrecoverable error.
    Failed,
    /// For future compatibility.
    #[serde(other)]
    Unknown,
}

/// `status` sub-resource update for a preview session CR.
///
/// Only fields that are set are included in the merge patch, allowing partial updates
/// without replacing the full status object.
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PreviewStatusUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    phase: Option<PreviewSessionPhase>,

    #[serde(skip_serializing_if = "Option::is_none")]
    started_at: Option<MicroTime>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pod_name: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    failure_message: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    expires_at: Option<MicroTime>,
}

impl PreviewStatusUpdate {
    /// Creates an empty status update.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets `.status.phase`.
    pub fn phase(mut self, phase: PreviewSessionPhase) -> Self {
        self.phase = Some(phase);
        self
    }

    /// Sets `.status.startedAt`.
    pub fn started_at(mut self, started_at: MicroTime) -> Self {
        self.started_at = Some(started_at);
        self
    }

    /// Sets `.status.podName`.
    pub fn pod_name(mut self, pod_name: String) -> Self {
        self.pod_name = Some(pod_name);
        self
    }

    /// Sets `.status.failureMessage`.
    pub fn failure_message(mut self, failure_message: String) -> Self {
        self.failure_message = Some(failure_message);
        self
    }

    /// Sets `.status.expiresAt`.
    pub fn expires_at(mut self, expires_at: MicroTime) -> Self {
        self.expires_at = Some(expires_at);
        self
    }

    /// Patches the status sub-resource with a merge patch.
    pub async fn patch(
        self,
        client: &Client,
        name: &str,
        namespace: &str,
    ) -> Result<(), kube::Error> {
        let api = Api::<PreviewSession>::namespaced(client.clone(), namespace);
        let data = json!({ "status": self });

        api.patch_status(name, &PatchParams::default(), &Patch::Merge(data))
            .await?;

        Ok(())
    }
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

/// Queue splitting configuration for preview environments.
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewQueueSplittingConfig {
    /// SQS queue splitting filters, keyed by queue ID.
    ///
    /// Each entry configures how messages from a specific SQS queue should be filtered
    /// for this preview session.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sqs_queue_filters: BTreeMap<QueueId, PreviewSqsFilter>,

    /// Kafka queue splitting filters, keyed by topic ID.
    ///
    /// Each value maps header names to regex patterns that messages must match.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub kafka_queue_filters: BTreeMap<QueueId, BTreeMap<String, String>>,
}

impl PreviewQueueSplittingConfig {
    /// Converts from the user's split queues config. Returns `None` when no queues are configured.
    pub fn from_config(value: &SplitQueuesConfig) -> Option<Self> {
        let mut sqs_queue_filters = BTreeMap::new();

        for (id, message_filter) in value.sqs() {
            sqs_queue_filters
                .entry(id.to_owned())
                .or_insert_with(PreviewSqsFilter::default)
                .message_filter = Some(message_filter.clone());
        }

        for (id, jq_filter) in value.sqs_jq_filters() {
            sqs_queue_filters
                .entry(id.to_owned())
                .or_insert_with(PreviewSqsFilter::default)
                .jq_filter = Some(jq_filter.to_owned());
        }

        let kafka_queue_filters: BTreeMap<_, _> = value
            .kafka()
            .map(|(id, filter)| (id.to_owned(), filter.clone()))
            .collect();

        if sqs_queue_filters.is_empty() && kafka_queue_filters.is_empty() {
            None
        } else {
            Some(Self {
                sqs_queue_filters,
                kafka_queue_filters,
            })
        }
    }
}

/// Per-queue SQS filter configuration for preview sessions.
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSqsFilter {
    /// Message attribute filter: a mapping from attribute names to regex patterns.
    ///
    /// Only messages whose attributes match **all** patterns will be delivered
    /// to the preview environment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_filter: Option<BTreeMap<String, String>>,

    /// A jq filter expression.
    ///
    /// For each SQS message, the jq filter runs on a JSON representation of the
    /// SQS `Message` object. If the jq program outputs `true`, the message is
    /// considered as matching.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jq_filter: Option<String>,
}

/// Database branching configuration for preview environments.
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewDbBranchingConfig {
    /// MySQL branch database names to use for this session.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mysql_branch_names: Vec<String>,

    /// PostgreSQL branch database names to use for this session.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pg_branch_names: Vec<String>,

    /// MongoDB branch database names to use for this session.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mongodb_branch_names: Vec<String>,

    /// MSSQL branch database names to use for this session.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mssql_branch_names: Vec<String>,
}

impl PreviewDbBranchingConfig {
    /// Returns `None` when all branch name lists are empty.
    #[cfg(feature = "client")]
    pub fn from_db_names(branch_db_names: BranchDbNames) -> Option<Self> {
        if branch_db_names.is_empty() {
            None
        } else {
            Some(Self {
                mysql_branch_names: branch_db_names.mysql,
                pg_branch_names: branch_db_names.pg,
                mongodb_branch_names: branch_db_names.mongodb,
                mssql_branch_names: branch_db_names.mssql,
            })
        }
    }
}
