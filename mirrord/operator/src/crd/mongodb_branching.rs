use std::{
    collections::{BTreeMap, HashMap},
    fmt::Formatter,
};

use k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime;
use kube::CustomResource;
use mirrord_config::{
    feature::database_branches::{MongodbBranchCollectionCopyConfig, MongodbBranchCopyConfig},
    target::Target,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::crd::session::SessionOwner;

#[derive(CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "dbs.mirrord.metalbear.co",
    version = "v1alpha1",
    kind = "MongodbBranchDatabase",
    status = "MongodbBranchDatabaseStatus",
    namespaced
)]
#[serde(rename_all = "camelCase")]
pub struct MongodbBranchDatabaseSpec {
    /// ID derived by mirrord CLI.
    pub id: String,
    /// Database connection info from the workload.
    pub connection_source: ConnectionSource,
    /// MongoDB database name.
    pub database_name: Option<String>,
    /// Target k8s resource to extract connection source info from.
    pub target: Target,
    /// The duration in seconds this branch database will live idling.
    pub ttl_secs: u64,
    /// MongoDB server image version, e.g. "7.0".
    pub mongodb_version: Option<String>,
    /// Options for copying data from source database to the branch.
    #[serde(default)]
    pub copy: BranchCopyConfig,
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

    /// Environment from resource reference in the the pod template.
    EnvFrom {
        container: Option<String>,
        variable: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MongodbBranchDatabaseStatus {
    pub phase: BranchDatabasePhase,
    /// Time when the branch database should be deleted.
    pub expire_time: MicroTime,
    /// Information of sessions that are using this branch database.
    #[serde(default)]
    pub session_info: HashMap<String, SessionInfo>,
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
    pub id: String,
    /// Owner info of the session.
    pub owner: SessionOwner,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BranchCopyConfig {
    /// The copy mode for the branch.
    pub mode: BranchCopyMode,

    /// An optional list of collections to copy with their filters.
    /// If not specified, all collections are copied (for `All` mode) or none (for `Empty` mode).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collections: Option<BTreeMap<String, CollectionCopyConfig>>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum BranchCopyMode {
    /// Create an empty database only.
    Empty,
    /// Create a database and copy collections' schema and data from the source database.
    /// Supports optional collection filters to copy specific collections or filter documents.
    All,
}

impl Default for BranchCopyConfig {
    fn default() -> Self {
        BranchCopyConfig {
            mode: BranchCopyMode::Empty,
            collections: Default::default(),
        }
    }
}

impl From<MongodbBranchCopyConfig> for BranchCopyConfig {
    fn from(config: MongodbBranchCopyConfig) -> Self {
        match config {
            MongodbBranchCopyConfig::Empty { collections } => BranchCopyConfig {
                mode: BranchCopyMode::Empty,
                collections: collections.map(|c| {
                    c.into_iter()
                        .map(|(name, config)| (name, config.into()))
                        .collect()
                }),
            },
            MongodbBranchCopyConfig::All { collections } => BranchCopyConfig {
                mode: BranchCopyMode::All,
                collections: collections.map(|c| {
                    c.into_iter()
                        .map(|(name, config)| (name, config.into()))
                        .collect()
                }),
            },
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CollectionCopyConfig {
    /// Data that matches the filter will be copied.
    /// For MongoDB, this filter is a JSON query document like `{"username": "alice"}`.
    pub filter: Option<String>,
}

impl From<MongodbBranchCollectionCopyConfig> for CollectionCopyConfig {
    fn from(config: MongodbBranchCollectionCopyConfig) -> Self {
        CollectionCopyConfig {
            filter: config.filter,
        }
    }
}
