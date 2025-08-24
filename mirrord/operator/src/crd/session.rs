use kube::CustomResource;
use mirrord_kube::api::kubernetes::AgentKubernetesConnectInfo;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize, de, ser};

fn serialize_config_hash<S>(hash: &u64, serializer: S) -> Result<S::Ok, S::Error>
where
    S: ser::Serializer,
{
    format!("{hash:x}").serialize(serializer)
}

fn deserialize_config_hash<'de, D>(d: D) -> Result<u64, D::Error>
where
    D: de::Deserializer<'de>,
{
    let str_value = String::deserialize(d)?;
    u64::from_str_radix(&str_value, 16).map_err(de::Error::custom)
}

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
    pub agents: Vec<MirrordClusterSessionAgentInfo>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MirrordClusterSessionAgentInfo {
    #[serde(
        deserialize_with = "deserialize_config_hash",
        serialize_with = "serialize_config_hash"
    )]
    pub config_hash: u64,

    pub ephemeral: bool,

    pub name: String,
    pub namespace: String,

    pub port: u16,

    pub target: SessionTarget,
}

impl From<MirrordClusterSessionAgentInfo> for AgentKubernetesConnectInfo {
    fn from(session_agent: MirrordClusterSessionAgentInfo) -> Self {
        AgentKubernetesConnectInfo {
            agent_port: session_agent.port,
            pod_name: session_agent.name,
            pod_namespace: session_agent.namespace,
        }
    }
}

impl From<&MirrordClusterSessionAgentInfo> for AgentKubernetesConnectInfo {
    fn from(session_agent: &MirrordClusterSessionAgentInfo) -> Self {
        AgentKubernetesConnectInfo {
            agent_port: session_agent.port,
            pod_name: session_agent.name.clone(),
            pod_namespace: session_agent.namespace.clone(),
        }
    }
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
        let info = MirrordClusterSessionAgentInfo {
            config_hash: 1337,
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
