use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::DatabaseBranchBaseConfig;

/// When configuring a branch for MSSQL, set `type` to `mssql`.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MssqlBranchConfig {
    /// <!--${internal}-->
    /// Shared fields documented once under the MongoDB branch config variant.
    #[serde(flatten)]
    pub base: DatabaseBranchBaseConfig,

    #[serde(default)]
    pub copy: MssqlBranchCopyConfig,
}

/// Users can choose from the following copy mode to bootstrap their MSSQL branch database:
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
#[serde(tag = "mode", rename_all = "lowercase", deny_unknown_fields)]
pub enum MssqlBranchCopyConfig {
    Empty {
        tables: Option<BTreeMap<String, MssqlBranchTableCopyConfig>>,
    },

    Schema {
        tables: Option<BTreeMap<String, MssqlBranchTableCopyConfig>>,
    },

    All,
}

impl Default for MssqlBranchCopyConfig {
    fn default() -> Self {
        MssqlBranchCopyConfig::Empty {
            tables: Default::default(),
        }
    }
}

pub type MssqlBranchTableCopyConfig = super::BranchItemCopyConfig;
