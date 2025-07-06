use chrono::{DateTime, Utc};
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "mirrord.metalbear.co",
    version = "v1alpha",
    kind = "MirrordSession"
)]
#[serde(rename_all = "camelCase")]
pub struct MirrordSessionSpec {
    /// Resources needed to report session metrics to the mirrord Jira app
    #[serde(default, skip_serializing_if = "JiraMetricsResources::is_empty")]
    pub jira_metrics_resources: JiraMetricsResources,

    /// Session's target
    pub target: SessionTarget,

    /// Owner of this session
    pub owner: MirrordSessionOwner,

    /// Start time of this session when actual websocket connection is first created.
    pub start_time: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MirrordSessionOwner {
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
    pub kind: String,
    pub namespace: String,
    pub name: Option<String>,
    pub container: Option<String>,
}

/// Resources needed to report session metrics to the mirrord Jira app
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct JiraMetricsResources {
    /// The Jira webhook URL, used to update total session time in the mirrord Jira app
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jira_webhook_url: Option<String>,

    /// The user's current git branch, used for sending session metrics to mirrord Jira app
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch_name: Option<String>,
}

impl JiraMetricsResources {
    pub fn is_empty(&self) -> bool {
        self.jira_webhook_url.is_none() && self.branch_name.is_none()
    }
}
