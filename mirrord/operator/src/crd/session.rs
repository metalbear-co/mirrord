use std::fmt;

use kube::CustomResource;
use mirrord_config::feature::network::incoming::ConcurrentSteal;
use mirrord_kube::api::kubernetes::AgentKubernetesConnectInfo;
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

/// Limit for concurrently used agents in a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentLimit {
    /// Represents a hard constant limit. The session should never use more agents than this
    /// number, regardless of available target pods.
    Constant(u32),
    /// Represents desired percentage of available target pods that should have agent attached
    /// (rounded up).
    Percentage(u32),
}

impl AgentLimit {
    /// No limit, all target pods should be covered by agents.
    pub const ALL: Self = Self::Percentage(100);

    /// Calculates maximum number of agents that should be used, given count of available target
    /// pods.
    pub fn calculate_max(&self, available: usize) -> usize {
        match self {
            Self::Constant(limit) => {
                let limit = usize::try_from(*limit).unwrap_or(usize::MAX);
                std::cmp::min(limit, available)
            }
            Self::Percentage(percentage) => {
                let percentage = usize::try_from(*percentage).unwrap_or(usize::MAX);
                (available * percentage).div_ceil(100)
            }
        }
    }
}

impl Default for AgentLimit {
    fn default() -> Self {
        Self::ALL
    }
}

impl Serialize for AgentLimit {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            AgentLimit::Constant(limit) => {
                let as_string = limit.to_string();
                serializer.serialize_some(&as_string)
            }
            AgentLimit::Percentage(limit) => {
                let as_string = format!("{limit}p");
                serializer.serialize_str(&as_string)
            }
        }
    }
}

impl<'de> Deserialize<'de> for AgentLimit {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        /// Empty helper type for deserializing [`AgentLimit`].
        struct AgentLimitVisitor;

        impl de::Visitor<'_> for AgentLimitVisitor {
            type Value = AgentLimit;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.collect_str("a positive const (e.g '1') or percentage (e.g '50p') limit")
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                let result = match v.strip_suffix('p') {
                    Some(v) => v
                        .parse::<u32>()
                        .ok()
                        .filter(|value| *value > 0 && *value <= 100)
                        .map(AgentLimit::Percentage),
                    None => v
                        .parse::<u32>()
                        .ok()
                        .filter(|value| *value > 0)
                        .map(AgentLimit::Constant),
                };

                result.ok_or_else(|| E::invalid_value(de::Unexpected::Str(v), &self))
            }
        }

        deserializer.deserialize_str(AgentLimitVisitor)
    }
}

impl fmt::Display for AgentLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Constant(limit) => write!(f, "{limit}"),
            Self::Percentage(percentage) => write!(f, "{percentage}%"),
        }
    }
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

    /// Agent Limit
    #[serde(default)]
    #[schemars(with = "String")]
    pub agent_limit: AgentLimit,

    /// State of concurrent steal
    #[serde(default)]
    pub on_concurrent_steal: ConcurrentSteal,

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<i32>,

    pub message: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MirrordClusterSessionAgent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_info: Option<AgentKubernetesConnectInfo>,

    #[serde(skip_serializing_if = "Option::is_none")]
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
    fn agent_limit_calculate() {
        assert_eq!(AgentLimit::Constant(2).calculate_max(1), 1);
        assert_eq!(AgentLimit::Constant(2).calculate_max(3), 2);
        assert_eq!(AgentLimit::Percentage(40).calculate_max(100), 40);
        assert_eq!(AgentLimit::Percentage(1).calculate_max(1), 1);
        assert_eq!(AgentLimit::Percentage(50).calculate_max(7), 4);
    }

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
