use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    BranchBaseConfig, BranchPodConfig, DatabaseSourceConfig, IamAuthConfig,
    SqlBranchMigrationsConfig,
};

/// When configuring a branch for PostgreSQL, set `type` to `pg`.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PgBranchConfig {
    #[serde(flatten)]
    pub base: BranchBaseConfig,

    #[serde(flatten)]
    pub pod: BranchPodConfig,

    #[serde(flatten)]
    pub database: DatabaseSourceConfig,

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

    /// #### feature.db_branches[].query_params (type: pg) {#feature-db_branches-pg-query_params}
    ///
    /// Query parameters applied to the branch connection handed to your application - the
    /// reconstructed connection URL and, in params mode, the matching environment variables.
    /// Values win over mirrord's own defaults. They only affect the branch connection; the
    /// source database connection used for the copy is not changed.
    ///
    /// The common use is `sslmode`: a source like GCP Cloud SQL may require
    /// `?sslmode=require`, while the branch pod mirrord creates serves no TLS, so the branch
    /// connection needs `{ "sslmode": "disable" }` (this is also mirrord's default for
    /// non-TLS branch pods).
    ///
    /// ```json
    /// {
    ///   "feature": {
    ///     "db_branches": [
    ///       {
    ///         "type": "pg",
    ///         "query_params": { "sslmode": "disable" }
    ///       }
    ///     ]
    ///   }
    /// }
    /// ```
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub query_params: BTreeMap<String, String>,

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
