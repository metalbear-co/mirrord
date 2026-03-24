use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::DatabaseBranchBaseConfig;

/// When configuring a branch for MySQL, set `type` to `mysql`.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
pub struct MysqlBranchConfig {
    #[serde(flatten)]
    pub base: DatabaseBranchBaseConfig,

    #[serde(default)]
    pub copy: MysqlBranchCopyConfig,
}

/// Users can choose from the following copy mode to bootstrap their MySQL branch database:
///
/// - Empty
///
///   Creates an empty database. If the source DB connection options are found from the chosen
///   target, mirrord operator extracts the database name and create an empty DB. Otherwise, mirrord
///   operator looks for the `name` field from the branch DB config object. This option is useful
///   for users that run DB migrations themselves before starting the application.
///
/// - Schema
///
///   Creates an empty database and copies schema of all tables.
///
/// - All
///
///   Copies both schema and data of all tables. This option shall only be used
///   when the data volume of the source database is minimal.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum MysqlBranchCopyConfig {
    Empty {
        tables: Option<BTreeMap<String, MysqlBranchTableCopyConfig>>,
    },

    Schema {
        tables: Option<BTreeMap<String, MysqlBranchTableCopyConfig>>,
    },

    All,
}

impl Default for MysqlBranchCopyConfig {
    fn default() -> Self {
        MysqlBranchCopyConfig::Empty {
            tables: Default::default(),
        }
    }
}

pub type MysqlBranchTableCopyConfig = super::BranchItemCopyConfig;
