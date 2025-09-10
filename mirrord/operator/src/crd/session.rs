use kube::CustomResource;
use mirrord_kube::api::kubernetes::AgentKubernetesConnectInfo;
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
    /// Resources needed to report session metrics to the mirrord Jira app
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jira_metrics_resources: Option<JiraMetricsResources>,

    /// Owner of this session
    pub owner: MirrordClusterSessionOwner,

    /// Session's [`Target`](mirrord_config::target::Target)
    pub target: SessionTarget,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MirrordClusterSessionStatus {
    /// List of registed agents
    #[serde(default)]
    pub agents: Vec<MirrordClusterSessionAgent>,
}

#[derive(Clone, Debug, Deserialize, Default, Eq, PartialEq, Serialize, JsonSchema)]
pub enum MirrordClusterSessionAgentPhase {
    #[default]
    Pending,

    Created,

    Running,

    Terminating,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionAgentError {
    #[serde(skip_serializing_if = Option::is_none)]
    pub code: Option<i32>,

    pub message: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MirrordClusterSessionAgent {
    #[serde(skip_serializing_if = Option::is_none)]
    pub connection_info: Option<AgentKubernetesConnectInfo>,

    #[serde(skip_serializing_if = Option::is_none)]
    pub error: Option<SessionAgentError>,

    #[serde(default)]
    pub phase: MirrordClusterSessionAgentPhase,

    pub target: SessionTarget,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
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
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
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
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct JiraMetricsResources {
    /// The Jira webhook URL, used to update total session time in the mirrord Jira app
    pub jira_webhook_url: String,

    /// The user's current git branch, used for sending session metrics to mirrord Jira app
    pub branch_name: String,
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn correct_serde_for_connection_info() {
        let info = MirrordClusterSessionAgent {
            ephemeral: false,
            name: "my-pod".into(),
            namespace: "my-namespace".into(),
            port: 9999,
            target: SessionTarget {
                api_version: "v1".into(),
                kind: "Pod".into(),
                namespace: "my-target-namespace".into(),
                name: Some("my-target".into()),
                container: None,
            },
        };

        let info_json = serde_json::to_string(&info).expect("should serialize");
        let cloned = serde_json::from_str(&info_json).expect("should deserialize");

        assert_eq!(info, cloned)
    }
}
