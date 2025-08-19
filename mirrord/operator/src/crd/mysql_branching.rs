use std::{collections::HashMap, fmt::Formatter};

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
    /// ID derived by mirrord CLI.
    pub id: String,
    /// Database connection info from the workload.
    pub connection_source: ConnectionSource,
    /// MySQL database name.
    pub database_name: String,
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
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ConnectionSourceKind {
    /// Environment variable with value defined directly in the pod template.
    Env {
        container: Option<String>,
        variable: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MysqlBranchDatabaseStatus {
    pub phase: BranchDatabasePhase,
    /// Time when the branch database should be deleted.
    pub expire_time: DateTime<Utc>,
    /// Information of sessions that are using this branch database.
    #[serde(default)]
    pub session_info: HashMap<u64, SessionInfo>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, Eq, PartialEq)]
pub enum BranchDatabasePhase {
    /// The controller is creating the branch database.
    Pending,
    /// The branch database is ready to use.
    Ready,
}

impl std::fmt::Display for BranchDatabasePhase {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            BranchDatabasePhase::Pending => write!(f, "Pending"),
            BranchDatabasePhase::Ready => write!(f, "Ready"),
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
