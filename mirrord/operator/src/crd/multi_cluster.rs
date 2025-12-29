use k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::session::{SessionOwner, SessionTarget};

/// Multi-cluster session coordinated by Envoy
#[derive(CustomResource, Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[kube(
    group = "operator.metalbear.co",
    version = "v1alpha",
    kind = "MirrordMultiClusterSession",
    status = "MirrordMultiClusterSessionStatus",
    namespaced
)]
#[serde(rename_all = "camelCase")]
pub struct MirrordMultiClusterSessionSpec {
    /// Owner of this session
    pub owner: SessionOwner,
    
    /// Kubernetes namespace for the session (in primary cluster)
    pub namespace: String,
    
    /// Logical target alias provided by admin (e.g., "x-crm")
    /// This resolves to multiple targets across clusters
    pub target_alias: String,
    
    /// Clusters to create sessions in
    /// Key: cluster name (e.g., "us-east-1")
    /// Value: target details for that cluster
    pub cluster_targets: HashMap<String, ClusterTarget>,
    
    /// Primary cluster where ephemeral resources will be created
    pub primary_cluster: String,
}

/// Target specification for a specific cluster in a multi-cluster session
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClusterTarget {
    /// Target in this cluster
    pub target: SessionTarget,
    
    /// Namespace in this cluster (might differ from primary)
    pub namespace: String,
}

/// Status of a multi-cluster session
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MirrordMultiClusterSessionStatus {
    /// Status of each cluster's private session
    /// Key: cluster name
    /// Value: status of private session in that cluster
    pub cluster_sessions: HashMap<String, ClusterSessionStatus>,
    
    /// Overall session state
    pub phase: MultiClusterSessionPhase,
    
    /// Last time when at least one cluster had an active connection
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connected_timestamp: Option<MicroTime>,
    
    /// If the session has been closed, describes the reason
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed: Option<String>,
}

/// Status of a private session in a specific cluster
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClusterSessionStatus {
    /// Name of the MirrordPrivateClusterSession resource
    pub session_name: String,
    
    /// Whether this cluster's session is ready
    pub ready: bool,
    
    /// Agent endpoint for this cluster (WebSocket URL)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_endpoint: Option<String>,
    
    /// Error message if session failed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Phase of multi-cluster session lifecycle
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
pub enum MultiClusterSessionPhase {
    #[default]
    /// Creating private sessions in remote clusters
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

/// Private interface session - created by Envoy in remote clusters
#[derive(CustomResource, Clone, Debug, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[kube(
    group = "operator.metalbear.co",
    version = "v1alpha",
    kind = "MirrordPrivateClusterSession",
    status = "MirrordPrivateClusterSessionStatus",
    namespaced
)]
#[serde(rename_all = "camelCase")]
pub struct MirrordPrivateClusterSessionSpec {
    /// Target to mirror in THIS cluster
    pub target: SessionTarget,
    
    /// Session ID from primary cluster (for correlation)
    pub primary_session_id: String,
    
    /// Namespace in this cluster
    pub namespace: String,
    
    /// Owner info (from primary)
    pub owner: SessionOwner,
    
    /// Name of the primary cluster (for debugging)
    pub primary_cluster: String,
}

/// Status of a private cluster session
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MirrordPrivateClusterSessionStatus {
    /// Agent pod name created in this cluster
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_pod: Option<String>,
    
    /// WebSocket endpoint to connect to this cluster's agent
    /// Format: wss://<service>.<namespace>.svc.cluster.local:<port>
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_endpoint: Option<String>,
    
    /// Whether the session is ready
    pub ready: bool,
    
    /// Error if failed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    
    /// Last heartbeat from agent
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_heartbeat: Option<MicroTime>,
}

