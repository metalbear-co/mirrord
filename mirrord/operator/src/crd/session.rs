use std::{collections::BTreeMap, net::SocketAddr};

use k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(CustomResource, Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[kube(
    group = "mirrord.metalbear.co",
    version = "v1alpha",
    kind = "MirrordClusterSession",
    status = "MirrordClusterSessionStatus"
)]
#[serde(rename_all = "camelCase")]
pub struct MirrordClusterSessionSpec {
    /// Resources needed to report this session's metrics to the mirrord Jira app.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jira_metrics_resources: Option<JiraMetricsResources>,

    /// Owner of this session
    pub owner: SessionOwner,

    /// Namespace of this session.
    pub namespace: String,

    /// Target of this session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<SessionTarget>,

    /// mirrord features used in this session.
    pub features: SessionFeatures,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionOwner {
    /// Unique ID.
    pub user_id: String,
    /// Creator local username.
    pub username: String,
    /// Creator hostname.
    pub hostname: String,
    /// Creator Kubernetes username.
    pub k8s_username: String,
}

/// Resources needed to report session metrics to the mirrord Jira app
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionTarget {
    /// Target resource apiVersion
    pub api_version: String,
    /// Target resource kind.
    pub kind: String,
    /// Target resource name.
    pub name: String,
    /// Target container name.
    pub container: String,
}

/// Resources needed to report session metrics to the mirrord Jira app.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct JiraMetricsResources {
    /// The user's current git branch.
    pub branch_name: String,
}

/// Describes the set of mirrord features used in a mirrord session.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionFeatures {
    pub copy_target: SessionCopyTarget,
    pub sqs_splitting: SessionQueueSplitting,
    pub kafka_splitting: SessionQueueSplitting,
    pub mysql_branching: SessionMySqlBranching,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionCopyTarget {
    pub enabled: bool,
    pub scale_down: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionQueueSplitting {
    #[serde(skip_serializing_if = "BTreeMap::is_empty", default)]
    pub filters: BTreeMap<String, BTreeMap<String, String>>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionMySqlBranching {
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub branches: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MirrordClusterSessionStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connected_timestamp: Option<MicroTime>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copy_target: Option<SessionFeatureStatus<SessionCopiedPodReady>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sqs_splitting_status: Option<SessionFeatureStatus<SessionEnvOverrides>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kafka_splitting_status: Option<SessionFeatureStatus<SessionEnvOverrides>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mysql_branching_status: Option<SessionFeatureStatus<SessionEnvOverrides>>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub agents: Vec<SessionAgent>,
}

/// Generic status of preparing a session feature.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SessionFeatureStatus<T> {
    Ready(T),
    Error(FatalError),
}

/// Describes an output of preparing the copy target session feature - copied pod to use as a
/// target.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionCopiedPodReady {
    pub pod: String,
    pub container: String,
}

/// Describes an output of preparing a session feature - a list of environment overrides.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEnvOverrides {
    pub env_overrides: BTreeMap<String, String>,
}

/// Describes a fatal error that has occurred when preparing a session feature.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FatalError {
    pub code: u16,
    pub reason: String,
    pub message: String,
}

/// Describes a mirrord agent spawned for a session.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionAgent {
    /// Address on which the agent accepts mirrord-protocol connections.
    pub address: SocketAddr,
}
