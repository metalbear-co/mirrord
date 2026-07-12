use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    DatabaseBranchBaseConfig, SqlBranchMigrationsConfig, SubsetLimitsConfig, SubsetSeedConfig,
};
use crate::config::ConfigError;

/// When configuring a branch for MSSQL, set `type` to `mssql`.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MssqlBranchConfig {
    #[serde(flatten)]
    pub base: DatabaseBranchBaseConfig,

    #[serde(default)]
    pub copy: MssqlBranchCopyConfig,

    /// <!--${internal}-->
    /// Documented on `DatabaseBranchConfig` (shared across SQL engines).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migrations: Option<SqlBranchMigrationsConfig>,
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
///
/// - Subset
///
///   Copies the schema of all tables plus the rows related to the seed rows selected in
///   `tables`, discovered automatically through declared foreign keys: the seeds'
///   dependent rows (their children, transitively — a user's orders, the orders'
///   payments) plus everything those rows reference, so nothing dangles. Rows brought in
///   only as references stay passive — a shared product comes along, other customers'
///   orders of it do not. Seeds use structured `conditions` instead of raw SQL, e.g.
///   `{ "mode": "subset", "tables": { "users": { "conditions": [{ "column": "id", "op": "eq",
/// "value": 5 }] } } }`.
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

    Subset {
        /// Seed tables (optionally schema-qualified, default `dbo`) with structured
        /// conditions selecting the starting rows.
        tables: BTreeMap<String, SubsetSeedConfig>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limits: Option<SubsetLimitsConfig>,
    },
}

impl Default for MssqlBranchCopyConfig {
    fn default() -> Self {
        MssqlBranchCopyConfig::Empty {
            tables: Default::default(),
        }
    }
}

impl MssqlBranchCopyConfig {
    pub fn verify(&self) -> Result<(), ConfigError> {
        match self {
            Self::Subset { tables, .. } => {
                super::verify_subset_seeds(tables, "feature.db_branches[].copy.tables")
            }
            _ => Ok(()),
        }
    }
}

pub type MssqlBranchTableCopyConfig = super::BranchItemCopyConfig;
