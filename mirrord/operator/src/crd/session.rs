use std::{fmt, time::Duration};

use k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime;
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
    status = "MirrordClusterSessionStatus",
    printcolumn = r#"{"name":"USER ID", "type":"string", "description":"User unique ID..", "jsonPath":".spec.owner.userId"}"#,
    printcolumn = r#"{"name":"USERNAME", "type":"string", "description":"User local POSIX name.", "jsonPath":".spec.owner.username"}"#,
    printcolumn = r#"{"name":"HOSTNAME", "type":"string", "description":"User hostname.", "jsonPath":".spec.owner.hostname"}"#,
    printcolumn = r#"{"name":"K8S USER", "type":"string", "description":"User Kubernetes name.", "jsonPath":".spec.owner.k8sUsername"}"#,
    printcolumn = r#"{"name":"NAMESPACE", "type":"string", "description":"Namespace of the session.", "jsonPath":".spec.namespace"}"#,
    printcolumn = r#"{"name":"TARGET", "type":"string", "description":"Target of the session.", "jsonPath":".spec.target"}"#,
    printcolumn = r#"{"name":"STARTED AT", "type":"date", "description":"Time when the session was started.", "jsonPath":".metadata.creationTimestamp"}"#,
    printcolumn = r#"{"name":"CLOSED AT", "type":"date", "description":"Time when the session was closed.", "jsonPath":".metadata.deletionTimestamp"}"#,
    printcolumn = r#"{"name":"CLOSE REASON", "type":"string", "description":"Reason for which the session was closed.", "jsonPath":".status.closed.reason"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct MirrordClusterSessionSpec {
    /// Resources needed to report session metrics to the mirrord Jira app.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jira_metrics: Option<SessionJiraMetrics>,
    /// Owner of this session
    pub owner: SessionOwner,
    /// Kubernetes namespace of the session.
    pub namespace: String,
    /// Agent Limit
    #[serde(default)]
    #[schemars(with = "String")]
    pub agent_limit: AgentLimit,
    /// State of concurrent steal
    #[serde(default)]
    pub on_concurrent_steal: ConcurrentSteal,
    /// Target of the session.
    ///
    /// None for targetless sessions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<SessionTarget>,
}

/// Describes an owner of a mirrord session.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionOwner {
    /// Unique ID.
    pub user_id: String,
    /// Name of the POSIX user that executed the CLI command.
    pub username: String,
    /// Hostname of the machine where the CLI command was executed.
    pub hostname: String,
    /// Name of the Kubernetes user who's identity was assumed by the CLI.
    pub k8s_username: String,
}

/// Describes a target of a mirrord session.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionTarget {
    /// Kubernetes resource apiVersion.
    pub api_version: String,
    /// Kubernetes resource kind.
    pub kind: String,
    /// Kubernetes resource name.
    pub name: String,
    /// Name of the container defined in the Pod spec.
    pub container: String,
}

/// Resources needed to report session metrics to the mirrord Jira app.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionJiraMetrics {
    /// The user's current git branch.
    pub branch_name: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MirrordClusterSessionAgent {
    /// Agent connection info to target's agents.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_info: Option<AgentKubernetesConnectInfo>,
    /// Agent spawn error if there is one.
    pub error: Option<SessionError>,
    /// The phase of agent's pod.
    pub phase: Option<String>,
    /// Resolved agent target.
    pub target: SessionTarget,
}

/// Describes an owner of a mirrord session.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MirrordClusterSessionStatus {
    #[serde(default)]
    pub agents: Vec<MirrordClusterSessionAgent>,
    /// Last time when the session was observed to have an open user connection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connected_timestamp: Option<MicroTime>,
    /// If the session has been closed, describes the reason.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed: Option<SessionError>,
}

/// Describes the reason for with a mirrord session was closed.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionError {
    /// Short reason in PascalCase.
    pub reason: String,
    /// Optional human friendly message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
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
