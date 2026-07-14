use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::DatabaseBranchBaseConfig;

/// When configuring a branch for ClickHouse, set `type` to `clickhouse`.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClickhouseBranchConfig {
    #[serde(flatten)]
    pub base: DatabaseBranchBaseConfig,

    #[serde(default)]
    pub copy: ClickhouseBranchCopyConfig,
}

/// Users can choose from the following copy mode to bootstrap their ClickHouse branch database:
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
///   Creates an empty database and copies the schema of all tables.
///
/// - All
///
///   Copies both schema and data of all tables. This option shall only be used
///   when the data volume of the source database is minimal.
///
/// In `empty` and `schema` mode, a per-table `filter` (a ClickHouse `WHERE` clause) copies just the
/// matching rows of the listed tables.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase", deny_unknown_fields)]
pub enum ClickhouseBranchCopyConfig {
    Empty {
        tables: Option<BTreeMap<String, ClickhouseBranchTableCopyConfig>>,
    },

    Schema {
        tables: Option<BTreeMap<String, ClickhouseBranchTableCopyConfig>>,
    },

    All,
}

impl Default for ClickhouseBranchCopyConfig {
    fn default() -> Self {
        ClickhouseBranchCopyConfig::Empty {
            tables: Default::default(),
        }
    }
}

pub type ClickhouseBranchTableCopyConfig = super::BranchItemCopyConfig;
