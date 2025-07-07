use chrono::{DateTime, Utc};
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "mirrord.metalbear.co",
    version = "v1alpha",
    kind = "MirrordClusterSession"
)]
#[serde(rename_all = "camelCase")]
pub struct MirrordClusterSessionSpec {
    /// Resources needed to report session metrics to the mirrord Jira app
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jira_metrics_resources: Option<JiraMetricsResources>,

    /// Session's [`Target`](mirrord_config::target::Target)
    pub target: SessionTarget,

    /// Owner of this session
    pub owner: MirrordClusterSessionOwner,

    /// Start time of this session when actual websocket connection is first created.
    pub start_time: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MirrordClusterSessionOwner {
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
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionTarget {
    /// Target's Resource apiVersion
    pub api_version: String,

    /// Target's Resource Kind
    pub kind: String,

    /// Target Namespace
    pub namespace: String,

    /// Target Name (will be empty for targetless)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// Target Container
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
}

/// Resources needed to report session metrics to the mirrord Jira app
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct JiraMetricsResources {
    /// The Jira webhook URL, used to update total session time in the mirrord Jira app
    pub jira_webhook_url: String,

    /// The user's current git branch, used for sending session metrics to mirrord Jira app
    pub branch_name: String,
}
