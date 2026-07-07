use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{DatabaseBranchBaseConfig, IamAuthConfig, SqlBranchMigrationsConfig};

/// When configuring a branch for PostgreSQL, set `type` to `pg`.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PgBranchConfig {
    #[serde(flatten)]
    pub base: DatabaseBranchBaseConfig,

    #[serde(default)]
    pub copy: PgBranchCopyConfig,

    /// #### feature.db_branches[].connection_settings (type: pg) {#feature-db_branches-pg-connection_settings}
    ///
    /// PostgreSQL settings (GUCs) applied to every source connection mirrord opens while
    /// building the branch. Each entry is sent at connection startup via `PGOPTIONS`, so it
    /// is in effect before any schema dump or data copy runs.
    ///
    /// The common use is a Row-Level Security tenant variable: if a source table has an RLS
    /// policy that reads `current_setting('my.tenant')`, set `{ "my.tenant": "1234" }` here so
    /// the copy can read the rows. Other GUCs work too, e.g. `role` to assume a table owner, or
    /// `search_path`.
    ///
    /// Values are literal strings; the usual config templating (such as `{{ get_env(...) }}`)
    /// still applies before they are sent.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub connection_settings: BTreeMap<String, String>,

    /// #### feature.db_branches[].iam_auth (type: pg) {#feature-db_branches-pg-iam_auth}
    ///
    /// IAM authentication for the source database.
    /// Use this when your source database (AWS RDS, GCP Cloud SQL) requires IAM authentication
    /// instead of password-based authentication.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iam_auth: Option<IamAuthConfig>,

    /// <!--${internal}-->
    /// Documented on `DatabaseBranchConfig` (shared across SQL engines).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migrations: Option<SqlBranchMigrationsConfig>,
}

/// Users can choose from the following copy mode to bootstrap their PostgreSQL branch database.
///
/// All copy modes accept `dump_args`. When this field is set, it replaces the default `pg_dump`
/// arguments. The defaults are `--no-owner` and `--no-acl`; include them explicitly when
/// overriding if you want to preserve the default behavior. An empty list means no dump args.
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
pub enum PgBranchCopyConfig {
    Empty {
        tables: Option<BTreeMap<String, PgBranchTableCopyConfig>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dump_args: Option<Vec<String>>,
    },

    Schema {
        tables: Option<BTreeMap<String, PgBranchTableCopyConfig>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dump_args: Option<Vec<String>>,
    },

    All {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dump_args: Option<Vec<String>>,
    },
}

impl Default for PgBranchCopyConfig {
    fn default() -> Self {
        PgBranchCopyConfig::Empty {
            tables: Default::default(),
            dump_args: None,
        }
    }
}

pub type PgBranchTableCopyConfig = super::BranchItemCopyConfig;
