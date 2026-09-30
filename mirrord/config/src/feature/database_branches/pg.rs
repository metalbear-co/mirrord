use std::{
    collections::{BTreeMap, HashSet},
    iter,
};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    BranchBaseConfig, BranchPodConfig, ConnectionSource, DatabaseSourceConfig, IamAuthConfig,
    ParamSource, SingleOrVec, SqlBranchMigrationsConfig,
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

/// Longest database name PostgreSQL keeps, in bytes (`NAMEDATALEN - 1`). The server cuts
/// longer identifiers down to this length, so two long names sharing their first 63 bytes
/// would end up as the same database on the branch.
pub const POSTGRES_MAX_IDENTIFIER_BYTES: usize = 63;

/// The template databases every PostgreSQL server has. They exist on the branch pod before
/// the copy runs, so a copy named after one would restore into the template itself, and every
/// database created after it would start as a clone of that data.
pub const POSTGRES_TEMPLATE_DATABASES: [&str; 2] = ["template0", "template1"];

impl PgBranchConfig {
    /// Checks `additional_databases` against each other and against the branch's own
    /// database and connection:
    /// - every name is set, fits a PostgreSQL identifier and is unique;
    /// - no var that names a database (a URL or `database` param) is read by another connection,
    ///   since it can only point at one database on the branch;
    /// - no two connections set the same var to different literal values, since the CLI keeps one
    ///   value per var.
    ///
    /// Host, port, user and password vars may be shared: every database lives on the same
    /// branch pod, so they get the same branch-side value whichever connection they belong to.
    pub fn verify_additional_databases(&self) -> Result<(), ConfigError> {
        const FIELD: &str = "feature.db_branches[].additional_databases";

        let mut names = HashSet::new();
        for (index, database) in self.additional_databases.iter().enumerate() {
            let name = database.name.trim();
            if name.is_empty() {
                return Err(ConfigError::Conflict(format!(
                    "`{FIELD}[{index}].name` is empty. Set it to the name of a database on \
                     the source server."
                )));
            }

            if name.len() > POSTGRES_MAX_IDENTIFIER_BYTES {
                return Err(ConfigError::Conflict(format!(
                    "`{FIELD}[{index}].name` is {} bytes long; PostgreSQL database names are \
                     at most {POSTGRES_MAX_IDENTIFIER_BYTES} bytes, and longer names get cut \
                     short. Use the database's real name on the source server.",
                    name.len()
                )));
            }

            if POSTGRES_TEMPLATE_DATABASES.contains(&name) {
                return Err(ConfigError::Conflict(format!(
                    "`{FIELD}[{index}].name` is `{name}`, a PostgreSQL template database. A \
                     copy would restore into the template every later database is cloned \
                     from. Use a regular database.",
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
        }

        let connections = iter::once((
            "the branch's own `connection`".to_owned(),
            &self.database.connection,
        ))
        .chain(
            self.additional_databases
                .iter()
                .enumerate()
                .filter_map(|(index, database)| {
                    Some((
                        format!("`{FIELD}[{index}].connection`"),
                        database.connection.as_ref()?,
                    ))
                }),
        )
        .collect::<Vec<_>>();

        for (index, (owner, connection)) in connections.iter().enumerate() {
            for key in connection.database_specific_env_keys() {
                let shared_with =
                    connections
                        .iter()
                        .enumerate()
                        .find(|&(other, (_, other_connection))| {
                            other != index && other_connection.all_env_keys().contains(&key)
                        });
                if let Some((_, (other_owner, _))) = shared_with {
                    return Err(ConfigError::Conflict(format!(
                        "{owner} names its database through the env var `{key}`, which \
                         {other_owner} also reads. Give each database its own URL or \
                         database var; host, port, user and password vars may be shared."
                    )));
                }
            }
        }

        // On a branch that keeps the app's own credentials (pg full roles mode), only the
        // branch's own connection user gets a login. A connection reading that user var
        // but another password var (or the reverse) would log in as a user whose password
        // the branch never installed; the operator cannot fix half a pair, so the pair is
        // shared whole or not at all.
        let credential_vars = |connection: &ConnectionSource| {
            let params = connection.params()?;
            let first_var = |sources: &Option<SingleOrVec<ParamSource>>| {
                sources.as_ref()?.first()?.as_variable().map(str::to_owned)
            };
            Some((first_var(&params.user), first_var(&params.password)))
        };
        if let Some((primary_user, primary_password)) = credential_vars(&self.database.connection) {
            for (owner, connection) in connections.iter().skip(1) {
                let Some((user, password)) = credential_vars(connection) else {
                    continue;
                };
                let shares_user = user.is_some() && user == primary_user;
                let shares_password = password.is_some() && password == primary_password;
                if user.is_some() && password.is_some() && shares_user != shares_password {
                    let (shared, own) = if shares_user {
                        ("user", "password")
                    } else {
                        ("password", "user")
                    };
                    return Err(ConfigError::Conflict(format!(
                        "{owner} reads the same {shared} var as the branch's own `connection` \
                         but its own {own} var. On a branch that keeps the app's credentials \
                         only the branch's own user can log in, so share both the user and \
                         the password vars, or neither."
                    )));
                }
            }
        }

        let mut literals: BTreeMap<&str, (&str, &str)> = BTreeMap::new();
        for (owner, connection) in &connections {
            for (key, value) in connection.literal_values() {
                match literals.insert(key, (value, owner.as_str())) {
                    Some((previous, previous_owner)) if previous != value => {
                        return Err(ConfigError::Conflict(format!(
                            "{owner} sets the env var `{key}` to a different literal value \
                             than {previous_owner}. A var has one value in the session; use \
                             the same value or a different var."
                        )));
                    }
                    _ => {}
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

    /// Every database lives on the same branch pod, so a host, port, user or password var
    /// gets the same branch-side value for each of them and may be shared.
    #[test]
    fn additional_connection_may_share_host_port_and_credentials() {
        verify(json!([{
            "type": "pg",
            "connection": { "type": "env", "params": {
                "host": "DB_HOST", "port": "DB_PORT", "user": "DB_USER",
                "password": "DB_PASSWORD", "database": "DB_NAME"
            } },
            "additional_databases": [{
                "name": "analytics",
                "connection": { "type": "env", "params": {
                    "host": "DB_HOST", "port": "DB_PORT", "user": "DB_USER",
                    "password": "DB_PASSWORD", "database": "ANALYTICS_DB"
                } }
            }]
        }]))
        .expect("shared host/port/credential vars point at the same branch pod");
    }

    /// A database var can only name one database on the branch.
    #[test]
    fn additional_connection_sharing_the_database_var_is_rejected() {
        let message = conflict_message(verify(json!([{
            "type": "pg",
            "connection": { "type": "env", "params": { "host": "DB_HOST", "database": "DB_NAME" } },
            "additional_databases": [{
                "name": "analytics",
                "connection": { "type": "env", "params": { "host": "DB_HOST", "database": "DB_NAME" } }
            }]
        }])));
        assert!(message.contains("`DB_NAME`"), "{message}");
    }

    /// PostgreSQL cuts longer identifiers to 63 bytes, where two long names could collide.
    #[test]
    fn additional_database_name_longer_than_an_identifier_is_rejected() {
        let config = |name: String| {
            json!([{
                "type": "pg",
                "connection": { "url": "DATABASE_URL" },
                "additional_databases": [{ "name": name }]
            }])
        };

        verify(config("a".repeat(POSTGRES_MAX_IDENTIFIER_BYTES)))
            .expect("a name of exactly 63 bytes fits");
        let message = conflict_message(verify(config(
            "a".repeat(POSTGRES_MAX_IDENTIFIER_BYTES + 1),
        )));
        assert!(message.contains("64 bytes long"), "{message}");
    }

    /// The CLI keeps one literal value per env var, so two connections setting one var to
    /// different values would silently lose one; the same value is fine.
    #[test]
    fn conflicting_literal_values_across_connections_are_rejected() {
        let config = |additional_sslmode: &str| {
            json!([{
                "type": "pg",
                "connection": { "type": "env", "params": {
                    "host": "DB_HOST",
                    "database": "DB_NAME",
                    "sslmode": { "env_var_name": "PGSSLMODE", "value": "require" }
                } },
                "additional_databases": [{
                    "name": "analytics",
                    "connection": { "type": "env", "params": {
                        "host": "DB_HOST",
                        "database": "ANALYTICS_DB",
                        "sslmode": { "env_var_name": "PGSSLMODE", "value": additional_sslmode }
                    } }
                }]
            }])
        };

        verify(config("require")).expect("the same literal value may be repeated");
        let message = conflict_message(verify(config("disable")));
        assert!(
            message.contains("`PGSSLMODE`") && message.contains("different literal value"),
            "{message}"
        );
    }

    /// A template database pre-exists on the branch pod, so a copy named after it would
    /// restore into the template and leak into every database created after it.
    #[test]
    fn additional_database_named_after_a_template_is_rejected() {
        for template in POSTGRES_TEMPLATE_DATABASES {
            let message = conflict_message(verify(json!([{
                "type": "pg",
                "connection": { "url": "DATABASE_URL" },
                "additional_databases": [{ "name": template }]
            }])));
            assert!(
                message.contains(&format!("`{template}`")) && message.contains("template"),
                "{message}"
            );
        }
    }

    /// Sharing only one of the user and password vars with the branch's own connection
    /// would leave the other pointing at credentials the branch never installed.
    #[test]
    fn additional_connection_sharing_half_the_credentials_is_rejected() {
        let config = |user_var: &str, password_var: &str| {
            json!([{
                "type": "pg",
                "connection": { "type": "env", "params": {
                    "host": "DB_HOST", "user": "DB_USER", "password": "DB_PASSWORD",
                    "database": "DB_NAME"
                } },
                "additional_databases": [{
                    "name": "analytics",
                    "connection": { "type": "env", "params": {
                        "host": "DB_HOST", "user": user_var, "password": password_var,
                        "database": "ANALYTICS_DB"
                    } }
                }]
            }])
        };

        verify(config("DB_USER", "DB_PASSWORD")).expect("the whole pair may be shared");
        verify(config("ANALYTICS_USER", "ANALYTICS_PASSWORD")).expect("a pair of its own is fine");
        for (user_var, password_var) in [
            ("DB_USER", "ANALYTICS_PASSWORD"),
            ("ANALYTICS_USER", "DB_PASSWORD"),
        ] {
            let message = conflict_message(verify(config(user_var, password_var)));
            assert!(
                message.contains("share both the user and the password vars"),
                "{user_var}/{password_var}: {message}"
            );
        }
    }
}
