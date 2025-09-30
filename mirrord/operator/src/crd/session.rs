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
    /// Resources needed to report session metrics to the mirrord Jira app.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jira_metrics_resources: Option<JiraMetricsResources>,

    /// Owner of this session
    pub owner: SessionOwner,

    /// Kubernetes namespace of the session.
    pub namespace: String,

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
pub struct JiraMetricsResources {
    /// The user's current git branch.
    pub branch_name: String,
}

/// Describes an owner of a mirrord session.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MirrordClusterSessionStatus {
    /// Last time when the session was observed to have an open user connection.
    pub connected_timestamp: Option<MicroTime>,
}
