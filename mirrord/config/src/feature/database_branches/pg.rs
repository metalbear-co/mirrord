use std::collections::{BTreeMap, HashSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    BranchBaseConfig, BranchPodConfig, ConnectionSource, DatabaseSourceConfig, IamAuthConfig,
    SqlBranchMigrationsConfig,
};
use crate::config::ConfigError;

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

    /// #### feature.db_branches[].additional_databases (type: pg) {#feature-db_branches-pg-additional_databases}
    ///
    /// More databases from the same source server, copied into the same branch server. Use
    /// it when your application talks to several databases on one PostgreSQL server: every
    /// database lands on one branch pod, so the application keeps using a single host.
    ///
    /// Each entry is dumped with the source connection of this branch (same host, port,
    /// user, password, TLS, `iam_auth` and `connection_settings`); only the database name
    /// differs. The database gets the same name on the branch.
    ///
    /// - `name`: the database name on the source server. Must differ from the branch's own
    ///   database and from every other entry.
    /// - `connection` (optional): how the application reaches this database, in the same shape as
    ///   [`connection`](#feature-db_branches-sql-connection). mirrord points it at the branch with
    ///   this database's name. Without it, the database is only created and copied, and the
    ///   application switches to it on the branch host by itself.
    /// - `copy` (optional): copy mode and table filters for this database, in the same shape as
    ///   the branch's own `copy`. Defaults to `empty`.
    ///
    /// ```json
    /// {
    ///   "feature": {
    ///     "db_branches": [
    ///       {
    ///         "type": "pg",
    ///         "connection": { "url": { "type": "env", "variable": "DATABASE_URL" } },
    ///         "copy": { "mode": "all" },
    ///         "additional_databases": [
    ///           {
    ///             "name": "analytics",
    ///             "connection": { "url": { "type": "env", "variable": "ANALYTICS_DATABASE_URL" } },
    ///             "copy": { "mode": "schema" }
    ///           }
    ///         ]
    ///       }
    ///     ]
    ///   }
    /// }
    /// ```
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_databases: Vec<PgAdditionalDatabaseConfig>,
}

/// <!--${internal}-->
/// One more database copied into a PostgreSQL branch. Documented on
/// `PgBranchConfig::additional_databases`.
#[derive(Clone, Debug, Eq, PartialEq, JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PgAdditionalDatabaseConfig {
    /// Database name on the source server, reused on the branch.
    pub name: String,

    /// How the application connects to this database. mirrord points it at the branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection: Option<ConnectionSource>,

    /// Copy mode and table filters for this database.
    #[serde(default)]
    pub copy: PgBranchCopyConfig,
}

impl PgBranchConfig {
    /// Checks `additional_databases` against each other and against the branch's own
    /// database: every name must be set and unique, and no two connections may read the
    /// same env var, or one redirect would overwrite the other.
    pub fn verify_additional_databases(&self) -> Result<(), ConfigError> {
        const FIELD: &str = "feature.db_branches[].additional_databases";

        let mut names = HashSet::new();
        let mut primary_keys = Vec::new();
        self.database.connection.collect_env_keys(&mut primary_keys);
        let mut env_owners: BTreeMap<&str, String> = primary_keys
            .into_iter()
            .map(|key| (key, "the branch's own `connection`".to_owned()))
            .collect();

        for (index, database) in self.additional_databases.iter().enumerate() {
            let name = database.name.trim();
            if name.is_empty() {
                return Err(ConfigError::Conflict(format!(
                    "`{FIELD}[{index}].name` is empty. Set it to the name of a database on \
                     the source server."
                )));
            }

            if self.database.name.as_deref() == Some(name) {
                return Err(ConfigError::Conflict(format!(
                    "`{FIELD}[{index}].name` is `{name}`, which is already the branch's own \
                     database (`feature.db_branches[].name`). Remove the entry, the branch \
                     copies that database anyway."
                )));
            }

            if !names.insert(name) {
                return Err(ConfigError::Conflict(format!(
                    "`{FIELD}` lists `{name}` more than once. Keep one entry per database."
                )));
            }

            let Some(connection) = &database.connection else {
                continue;
            };
            let mut keys = Vec::new();
            connection.collect_env_keys(&mut keys);
            for key in keys {
                let owner = format!("`{FIELD}[{index}].connection`");
                if let Some(previous) = env_owners.insert(key, owner.clone()) {
                    return Err(ConfigError::Conflict(format!(
                        "{owner} reads the env var `{key}`, which {previous} already reads. \
                         Each database needs its own env vars, or mirrord cannot point them \
                         at different databases."
                    )));
                }
            }
        }

        Ok(())
    }
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        config::ConfigContext,
        feature::database_branches::{DatabaseBranchConfig, DatabaseBranchesConfig},
    };

    fn branches(value: serde_json::Value) -> DatabaseBranchesConfig {
        serde_json::from_value(value).expect("the branch config should parse")
    }

    fn only_branch(config: &DatabaseBranchesConfig) -> &DatabaseBranchConfig {
        let [branch] = config.as_slice() else {
            panic!("expected one branch, got {config:?}");
        };
        branch
    }

    fn verify(value: serde_json::Value) -> Result<(), ConfigError> {
        branches(value).verify(&mut ConfigContext::default())
    }

    fn conflict_message(result: Result<(), ConfigError>) -> String {
        match result {
            Err(ConfigError::Conflict(message)) => message,
            other => panic!("expected a conflict error, got {other:?}"),
        }
    }

    /// One pg entry with two more databases: each keeps its own connection and copy mode,
    /// an entry without `connection` or `copy` gets none and `empty`, and the config passes
    /// verification.
    #[test]
    fn additional_databases_parse_with_their_own_connection_and_copy() {
        let config = branches(json!([{
            "type": "pg",
            "connection": { "url": { "type": "env", "variable": "DATABASE_URL" } },
            "copy": { "mode": "all" },
            "additional_databases": [
                {
                    "name": "analytics",
                    "connection": { "url": { "type": "env", "variable": "ANALYTICS_URL" } },
                    "copy": { "mode": "schema", "tables": { "events": { "filter": "id < 10" } } }
                },
                { "name": "audit" }
            ]
        }]));
        config
            .verify(&mut ConfigContext::default())
            .expect("distinct databases with distinct env vars are valid");

        let branch = only_branch(&config);
        let DatabaseBranchConfig::Pg(pg) = branch else {
            panic!("expected a pg branch, got {branch:?}");
        };
        let [analytics, audit] = pg.additional_databases.as_slice() else {
            panic!("expected two additional databases, got {pg:?}");
        };
        assert_eq!(analytics.name, "analytics");
        assert!(matches!(
            &analytics.copy,
            PgBranchCopyConfig::Schema { tables: Some(tables), .. } if tables.contains_key("events")
        ));
        assert_eq!(audit.connection, None);
        assert_eq!(audit.copy, PgBranchCopyConfig::default());

        // The operator redirects the additional connection too, so a local
        // `feature.env.override` on it must be rejected like one on the primary.
        assert_eq!(
            branch.connection_env_keys(),
            vec!["DATABASE_URL", "ANALYTICS_URL"]
        );
    }

    /// Omitting the field keeps today's single-database branch and serializes without it,
    /// so configs sent to the operator are unchanged for everyone not using it.
    #[test]
    fn no_additional_databases_serializes_without_the_field() {
        let config = branches(json!([{
            "type": "pg",
            "connection": { "url": "DATABASE_URL" }
        }]));
        let branch = only_branch(&config);
        assert!(branch.pg_additional_databases().is_empty());
        let serialized = serde_json::to_value(branch).unwrap();
        assert!(
            serialized.get("additional_databases").is_none(),
            "{serialized}"
        );
    }

    #[test]
    fn duplicate_additional_database_is_rejected() {
        let message = conflict_message(verify(json!([{
            "type": "pg",
            "connection": { "url": "DATABASE_URL" },
            "additional_databases": [{ "name": "analytics" }, { "name": "analytics" }]
        }])));
        assert!(message.contains("`analytics` more than once"), "{message}");
    }

    #[test]
    fn additional_database_named_like_the_branch_database_is_rejected() {
        let message = conflict_message(verify(json!([{
            "type": "pg",
            "name": "app",
            "connection": { "url": "DATABASE_URL" },
            "additional_databases": [{ "name": "app" }]
        }])));
        assert!(
            message.contains("additional_databases[0].name") && message.contains("`app`"),
            "{message}"
        );
    }

    #[test]
    fn empty_additional_database_name_is_rejected() {
        let message = conflict_message(verify(json!([{
            "type": "pg",
            "connection": { "url": "DATABASE_URL" },
            "additional_databases": [{ "name": "  " }]
        }])));
        assert!(
            message.contains("additional_databases[0].name` is empty"),
            "{message}"
        );
    }

    /// Two connections reading one env var cannot both be pointed at their own database: the
    /// second redirect would silently overwrite the first.
    #[test]
    fn additional_connection_sharing_an_env_var_is_rejected() {
        let message = conflict_message(verify(json!([{
            "type": "pg",
            "connection": { "url": "DATABASE_URL" },
            "additional_databases": [{
                "name": "analytics",
                "connection": { "type": "env", "params": { "host": "DB_HOST", "database": "DATABASE_URL" } }
            }]
        }])));
        assert!(
            message.contains("`DATABASE_URL`") && message.contains("the branch's own"),
            "{message}"
        );
    }

    /// The field belongs to PostgreSQL only; other engines keep denying it.
    #[test]
    fn additional_databases_are_unknown_to_other_engines() {
        let result = serde_json::from_value::<DatabaseBranchesConfig>(json!([{
            "type": "mysql",
            "connection": { "url": "DATABASE_URL" },
            "additional_databases": [{ "name": "analytics" }]
        }]));
        assert!(result.is_err(), "mysql must reject additional_databases");
    }
}
