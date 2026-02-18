use std::collections::HashMap;

use k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::session::{SessionOwner, SessionTarget};

/// Multi-cluster session coordinated by Envoy on Primary cluster.
#[derive(CustomResource, Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[kube(
    group = "operator.metalbear.co",
    version = "v1alpha",
    kind = "MirrordMultiClusterSession",
    status = "MirrordMultiClusterSessionStatus"
)]
#[serde(rename_all = "camelCase")]
pub struct MirrordMultiClusterSessionSpec {
    /// Owner of this session
    pub owner: SessionOwner,

    /// Kubernetes namespace for the session (in workload clusters)
    pub namespace: String,

    /// Target same for all workload clusters
    pub target: SessionTarget,

    /// Target identifier from user's mirrord config (e.g., "deployment.my-app")
    /// Format: "{kind}.{name}" where kind (deployment, pod, etc.)
    pub target_alias: String,

    /// List of cluster names to create sessions in
    pub clusters: Vec<String>,

    /// Primary cluster
    pub primary_cluster: String,

    /// Default cluster for stateful operations (db_branches, etc.)
    /// The primary cluster will be used as the default cluster when a default cluster is not
    /// specified.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_cluster: Option<String>,
}

/// Status of a multi-cluster session
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MirrordMultiClusterSessionStatus {
    /// Status of each cluster's private session
    /// Key: cluster name
    /// Value: status of private session in that cluster
    #[serde(default)]
    pub cluster_sessions: HashMap<String, ClusterSessionStatus>,

    /// Overall session state
    #[serde(default)]
    pub phase: MultiClusterSessionPhase,

    /// Last time when at least one cluster had an active connection
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connected_timestamp: Option<MicroTime>,

    /// If the session has been closed, describes the reason
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed: Option<String>,
}

/// Status of a cluster session within a multi-cluster session
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClusterSessionStatus {
    /// Name of the MirrordClusterSession resource in this cluster
    pub session_name: String,

    /// Whether this cluster's session is ready
    pub ready: bool,

    /// Agent endpoint for this cluster (WebSocket URL)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_endpoint: Option<String>,

    /// Error message if session failed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,

    /// Last heartbeat from agent
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_heartbeat: Option<MicroTime>,
}

/// Phase of multi-cluster session lifecycle
#[derive(
    Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, JsonSchema, strum_macros::Display,
)]
pub enum MultiClusterSessionPhase {
    #[default]
    /// Creating sessions in remote clusters
    Initializing,

    /// Waiting for all cluster sessions to become ready
    Pending,

    /// All cluster sessions are ready and connected
    Ready,

    /// One or more cluster sessions failed
    Failed,

    /// Session is being cleaned up
    Terminating,
}
