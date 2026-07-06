use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{DatabaseBranchBaseConfig, IamAuthConfig, SqlBranchMigrationsConfig};

/// When configuring a branch for MySQL, set `type` to `mysql`.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MysqlBranchConfig {
    #[serde(flatten)]
    pub base: DatabaseBranchBaseConfig,

    #[serde(default)]
    pub copy: MysqlBranchCopyConfig,

    /// #### feature.db_branches[].iam_auth (type: mysql) {#feature-db_branches-mysql-iam_auth}
    ///
    /// IAM authentication for the source database.
    /// Use this when your source database (AWS RDS, GCP Cloud SQL) requires IAM authentication
    /// instead of password-based authentication.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iam_auth: Option<IamAuthConfig>,

    /// #### feature.db_branches[].migrations (type: mysql, pg, mssql, clickhouse) {#feature-db_branches-sql-migrations}
    ///
    /// Schema migrations to run on the branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migrations: Option<SqlBranchMigrationsConfig>,
}

/// Users can choose from the following copy mode to bootstrap their MySQL branch database.
///
/// All copy modes accept `dump_args`. When this field is set, it replaces the default
/// `mysqldump` arguments. The defaults are `--single-transaction` and `--no-tablespaces`;
/// include them explicitly when overriding if you want to preserve the default behavior. An
/// empty list means no dump args.
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
///   Copies both schema and data of all tables. This option shall only be used when the data volume
///   of the source database is minimal.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase", deny_unknown_fields)]
pub enum MysqlBranchCopyConfig {
    Empty {
        tables: Option<BTreeMap<String, MysqlBranchTableCopyConfig>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dump_args: Option<Vec<String>>,
    },

    Schema {
        tables: Option<BTreeMap<String, MysqlBranchTableCopyConfig>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dump_args: Option<Vec<String>>,
    },

    All {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dump_args: Option<Vec<String>>,
    },
}

impl Default for MysqlBranchCopyConfig {
    fn default() -> Self {
        MysqlBranchCopyConfig::Empty {
            tables: Default::default(),
            dump_args: None,
        }
    }
}

pub type MysqlBranchTableCopyConfig = super::BranchItemCopyConfig;
