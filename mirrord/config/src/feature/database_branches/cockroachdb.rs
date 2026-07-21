use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{DatabaseBranchBaseConfig, SqlBranchMigrationsConfig};

/// When configuring a branch for CockroachDB, set `type` to `cockroachdb`.
///
/// CockroachDB is PostgreSQL-wire-compatible, so the app connects to the branch with its
/// existing PostgreSQL driver. The branch itself is a `cockroachdb/cockroach` single node and
/// the copy uses CockroachDB-native tooling (`cockroach sql`, `SHOW CREATE ALL TABLES`,
/// `COPY ... TO/FROM STDOUT/STDIN WITH CSV`) rather than the PostgreSQL dump tools.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CockroachdbBranchConfig {
    #[serde(flatten)]
    pub base: DatabaseBranchBaseConfig,

    #[serde(default)]
    pub copy: CockroachdbBranchCopyConfig,

    /// <!--${internal}-->
    /// Documented on `DatabaseBranchConfig` (shared across SQL engines).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migrations: Option<SqlBranchMigrationsConfig>,
}

/// Users can choose from the following copy mode to bootstrap their CockroachDB branch database:
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
///   Creates an empty database and copies schema of all tables via `SHOW CREATE ALL TABLES`.
///
/// - All
///
///   Copies both schema and data of all tables. This option shall only be used
///   when the data volume of the source database is minimal.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase", deny_unknown_fields)]
pub enum CockroachdbBranchCopyConfig {
    Empty {
        tables: Option<BTreeMap<String, CockroachdbBranchTableCopyConfig>>,
    },

    Schema {
        tables: Option<BTreeMap<String, CockroachdbBranchTableCopyConfig>>,
    },

    All,
}

impl Default for CockroachdbBranchCopyConfig {
    fn default() -> Self {
        CockroachdbBranchCopyConfig::Empty {
            tables: Default::default(),
        }
    }
}

pub type CockroachdbBranchTableCopyConfig = super::BranchItemCopyConfig;
