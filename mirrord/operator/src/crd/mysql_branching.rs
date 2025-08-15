use std::fmt::Formatter;

use chrono::{DateTime, Utc};
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::crd::kube_target::KubeTarget;

#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "dbs.mirrord.metalbear.co",
    version = "v1alpha1",
    kind = "MysqlBranchDatabase",
    status = "MysqlBranchDatabaseStatus",
    namespaced
)]
#[serde(rename_all = "camelCase")]
pub struct MysqlBranchDatabaseSpec {
    /// Database connection info from the workload.
    pub connection_source: ConnectionSource,
    /// Target k8s resource to extract connection source info from.
    pub target: KubeTarget,
    /// The duration in seconds this branch database will live idling.
    pub ttl_secs: u64,
    /// MySQL server image version, e.g. "8.0".
    pub mysql_version: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ConnectionSource {
    /// A complete connection URL.
    Url(ConnectionSourceKind),
    /// A group of connection parameters.
    Parameters {
        host: ConnectionSourceKind,
        port: Option<ConnectionSourceKind>,
        username: ConnectionSourceKind,
        password: ConnectionSourceKind,
        database: ConnectionSourceKind,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ConnectionSourceKind {
    /// Environment variable with value defined directly in the pod template.
    DirectEnvVar(EnvVarLocation),
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvVarLocation {
    /// Name of the container.
    pub container: String,
    /// Name of the variable.
    pub variable: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MysqlBranchDatabaseStatus {
    pub phase: BranchDatabasePhase,
    /// Time when the branch database should be deleted.
    pub expire_time: DateTime<Utc>,
    /// Information of the session that's using this branch database.
    pub session_info: Option<SessionInfo>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub enum BranchDatabasePhase {
    /// The controller is creating the branch database.
    Pending,
    /// The branch database is ready to use.
    Ready,
    /// The branch database is in use by a session.
    InUse,
}

impl std::fmt::Display for BranchDatabasePhase {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            BranchDatabasePhase::Pending => write!(f, "Pending"),
            BranchDatabasePhase::Ready => write!(f, "Ready"),
            BranchDatabasePhase::InUse => write!(f, "InUse"),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    /// Session id.
    pub id: u64,
    /// Unique session user id.
    pub user_id: String,
    /// Session creator's k8s username.
    pub k8s_username: String,
    /// Session creator's local username.
    pub local_username: String,
}
