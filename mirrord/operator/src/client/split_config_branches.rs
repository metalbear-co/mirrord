//! `feature.db_branches: "*"` / `["id", ...]`: the entries come from the `dbBranches` on the
//! target's `MirrordSplitConfig`, resolved by the operator, and sessions started under one key
//! that point at the same entry share one branch. The pieces here are pure; the operator call
//! and the create-and-wait flow live in [`OperatorApi::prepare_branch_dbs`].
//!
//! [`OperatorApi::prepare_branch_dbs`]: crate::client::OperatorApi::prepare_branch_dbs

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    ops::Not,
};

use kube::ResourceExt;
use mirrord_config::feature::database_branches::DatabaseBranchConfig;

use crate::{
    client::error::OperatorApiError,
    crd::db_branching::{
        branch_database::{BranchDatabase, BranchDatabaseSpec, DialectConfig},
        split_config::ResolveSplitConfigDbBranchesResponse,
    },
};

/// Label carrying the session key on branches resolved from a `MirrordSplitConfig`, so a
/// session can find the other branches under its key and tell when two of them point at the
/// same source database.
pub const MIRRORD_SESSION_KEY_LABEL: &str = "mirrord-session-key";

/// The branch id of a `dbBranches` entry under a session key: the entry id plus the key, so
/// every service started under the key that points at the entry lands on one branch.
pub fn branch_id(entry_id: &str, session_key: &str) -> String {
    format!("{entry_id}-{session_key}")
}

/// The branches a `"*"` / ids request resolved to, as this session's inline entries.
#[derive(Debug, Default)]
pub struct ResolvedSplitConfigBranches {
    /// The entries, each carrying its branch id (see [`branch_id`]).
    pub entries: Vec<DatabaseBranchConfig>,
    /// The branch ids of those entries, for telling resolved branches from inline ones later
    /// in the flow.
    pub branch_ids: HashSet<String>,
}

/// Turns the operator's answer into inline branch entries under `session_key`.
///
/// Each entry is parsed with this CLI's config version, so a setting the CLI does not know is
/// an error naming the entry rather than a silently dropped field.
pub fn entries_from_response(
    response: &ResolveSplitConfigDbBranchesResponse,
    session_key: &str,
) -> Result<ResolvedSplitConfigBranches, OperatorApiError> {
    let split_configs = response.split_configs.join("`, `");
    let mut resolved = ResolvedSplitConfigBranches::default();
    for entry in &response.entries {
        let id = branch_id(&entry.id, session_key);
        let mut config = entry.config.clone();
        if let serde_json::Value::Object(fields) = &mut config {
            fields.insert("id".to_owned(), serde_json::Value::String(id.clone()));
        }
        let config: DatabaseBranchConfig = serde_json::from_value(config).map_err(|error| {
            OperatorApiError::SplitConfigDbBranchEntry {
                id: entry.id.clone(),
                split_configs: split_configs.clone(),
                error: error.to_string(),
            }
        })?;
        resolved.entries.push(config);
        resolved.branch_ids.insert(id);
    }
    Ok(resolved)
}

/// The copy mode a branch spec asks for, as the user spells it (`empty`, `schema`, `all`).
/// A generic branch has no mode: it either runs a copy Job or not.
pub fn copy_mode(spec: &BranchDatabaseSpec) -> Option<String> {
    let copy = match spec.dialect().ok()? {
        DialectConfig::Postgres(options) => serde_json::to_value(&options.copy),
        DialectConfig::Mysql(options) => serde_json::to_value(&options.copy),
        DialectConfig::Mariadb(options) => serde_json::to_value(&options.copy),
        DialectConfig::Mssql(options) => serde_json::to_value(&options.copy),
        DialectConfig::Clickhouse(options) => serde_json::to_value(&options.copy),
        DialectConfig::Cockroachdb(options) => serde_json::to_value(&options.copy),
        DialectConfig::Spanner(options) => serde_json::to_value(&options.copy),
        DialectConfig::Mongodb(options) => serde_json::to_value(&options.copy),
        DialectConfig::Redis(options) => serde_json::to_value(&options.copy),
        DialectConfig::Dynamodb(options) => serde_json::to_value(&options.copy),
        DialectConfig::S3(options) => serde_json::to_value(&options.copy),
        DialectConfig::Turbopuffer(options) => serde_json::to_value(&options.copy),
        DialectConfig::Generic(options) => {
            return Some(
                if options.copy.is_some() {
                    "job"
                } else {
                    "empty"
                }
                .to_owned(),
            );
        }
    };
    copy.ok()?
        .get("mode")?
        .as_str()
        .map(|mode| mode.to_lowercase())
}

/// A session attaching to a branch another service created under the same key must ask for
/// the copy mode the branch was built with: the data is already there, and a different mode
/// would mean a different branch. Everything else on the entry (`connection`, `version`,
/// `ttlSecs`) is the creator's call and is not compared.
pub fn check_attach_copy_mode(
    existing: &BranchDatabase,
    requested: &BranchDatabaseSpec,
) -> Result<(), OperatorApiError> {
    let (Some(existing_mode), Some(requested_mode)) =
        (copy_mode(&existing.spec), copy_mode(requested))
    else {
        return Ok(());
    };
    if existing_mode == requested_mode {
        return Ok(());
    }
    Err(OperatorApiError::BranchCopyModeMismatch {
        branch_id: existing.spec.id.clone(),
        existing_mode,
        requested_mode,
        creator: existing.spec.target.name.clone(),
    })
}

/// Warnings for branches of this session whose resolved source database is the one another
/// branch under the same key was copied from: two `dbBranches` entries with different ids
/// that point at one database give two copies of it, which is rarely what the admin meant.
///
/// `ours` are this session's branches, `siblings` every branch under the key (ours included).
/// Branches without a recorded source (older operators, or not yet resolved) are skipped.
pub fn same_source_warnings<'a>(
    ours: impl IntoIterator<Item = &'a BranchDatabase>,
    siblings: &[BranchDatabase],
) -> Vec<String> {
    let mut by_source: BTreeMap<String, Vec<&BranchDatabase>> = BTreeMap::new();
    for sibling in siblings {
        if let Some(source) = sibling
            .status
            .as_ref()
            .and_then(|status| status.source.as_ref())
        {
            by_source
                .entry(source.to_string())
                .or_default()
                .push(sibling);
        }
    }

    let mut warnings = Vec::new();
    let mut ours_by_name: HashMap<String, &BranchDatabase> = HashMap::new();
    for branch in ours {
        ours_by_name.insert(branch.name_any(), branch);
    }
    for (source, branches) in &by_source {
        for branch in branches {
            if ours_by_name.contains_key(&branch.name_any()).not() {
                continue;
            }
            for other in branches {
                if other.name_any() == branch.name_any() {
                    continue;
                }
                warnings.push(format!(
                    "branch `{}` resolved to the same database as `{}` ({}): {source}\n  help: If \
                     these should share a branch, give both SplitConfig entries the same id.",
                    branch.spec.id, other.spec.id, other.spec.target.name,
                ));
            }
        }
    }
    warnings
}

#[cfg(test)]
mod tests {
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime;
    use kube::api::ObjectMeta;

    use super::*;
    use crate::crd::{
        db_branching::{
            branch_database::{PostgresOptions, SqlBranchCopyConfig, SqlBranchCopyMode},
            core::{BranchDatabasePhase, BranchDatabaseStatus, BranchSourceInfo, ConnectionSource},
            split_config::ResolvedSplitConfigDbBranch,
        },
        session::KubeResourceTarget,
    };

    fn pg_branch(name: &str, id: &str, creator: &str, mode: SqlBranchCopyMode) -> BranchDatabase {
        BranchDatabase {
            metadata: ObjectMeta {
                name: Some(name.to_owned()),
                ..Default::default()
            },
            spec: BranchDatabaseSpec {
                id: id.to_owned(),
                connection_source: ConnectionSource::Url(
                    vec![crate::crd::db_branching::core::ConnectionSourceKind::Env {
                        container: None,
                        variable: "DATABASE_URL".to_owned(),
                    }]
                    .into(),
                ),
                database_name: None,
                target: KubeResourceTarget {
                    api_version: "apps/v1".to_owned(),
                    kind: "Deployment".to_owned(),
                    name: creator.to_owned(),
                    container: String::new(),
                },
                ttl_secs: 300,
                version: None,
                image: None,
                profile: None,
                postgres_options: Some(PostgresOptions {
                    copy: SqlBranchCopyConfig {
                        mode,
                        items: None,
                        dump_args: None,
                    },
                    iam_auth: None,
                    connection_settings: Default::default(),
                    query_params: Default::default(),
                    additional_databases: Default::default(),
                }),
                mysql_options: None,
                mariadb_options: None,
                mongodb_options: None,
                mssql_options: None,
                redis_options: None,
                dynamodb_options: None,
                spanner_options: None,
                clickhouse_options: None,
                cockroachdb_options: None,
                s3_options: None,
                turbopuffer_options: None,
                generic_options: None,
                migrations: None,
            },
            status: None,
        }
    }

    fn with_source(mut branch: BranchDatabase, host: &str, database: &str) -> BranchDatabase {
        branch.status = Some(BranchDatabaseStatus {
            pod_name: None,
            phase: BranchDatabasePhase::Ready,
            expire_time: MicroTime(Default::default()),
            session_info: Default::default(),
            error: None,
            migrations: None,
            copy: None,
            conditions: Vec::new(),
            source: Some(BranchSourceInfo {
                host: host.to_owned(),
                port: Some(5432),
                database: Some(database.to_owned()),
            }),
        });
        branch
    }

    /// The branch id is the entry id plus the session key, so every service under the key
    /// that points at the entry derives the same deterministic branch name.
    #[test]
    fn branch_id_is_entry_id_plus_session_key() {
        assert_eq!(branch_id("orders-pg", "a1b2c3"), "orders-pg-a1b2c3");
    }

    /// Resolved entries get their branch id written in, and a setting this CLI does not know
    /// names the entry and the configs it came from instead of being dropped.
    #[test]
    fn entries_from_response_set_the_branch_id_and_name_unknown_settings() {
        let response = ResolveSplitConfigDbBranchesResponse {
            entries: vec![ResolvedSplitConfigDbBranch {
                id: "orders-pg".to_owned(),
                config: serde_json::json!({
                    "type": "pg",
                    "connection": { "url": "DATABASE_URL" },
                    "copy": { "mode": "empty" }
                }),
            }],
            split_configs: vec!["cake-maker".to_owned()],
            warnings: Vec::new(),
        };
        let resolved = entries_from_response(&response, "a1b2c3").expect("entry parses");
        let [entry] = resolved.entries.as_slice() else {
            panic!(
                "one entry resolves to one branch, got {:?}",
                resolved.entries
            );
        };
        assert_eq!(
            entry.base().and_then(|base| base.id.as_deref()),
            Some("orders-pg-a1b2c3")
        );
        assert!(resolved.branch_ids.contains("orders-pg-a1b2c3"));

        let unknown = ResolveSplitConfigDbBranchesResponse {
            entries: vec![ResolvedSplitConfigDbBranch {
                id: "orders-pg".to_owned(),
                config: serde_json::json!({
                    "type": "pg",
                    "connection": { "url": "DATABASE_URL" },
                    "from_the_future": true
                }),
            }],
            split_configs: vec!["cake-maker".to_owned()],
            warnings: Vec::new(),
        };
        let error = entries_from_response(&unknown, "a1b2c3")
            .expect_err("an unknown setting is an error")
            .to_string();
        assert!(
            error.contains("orders-pg") && error.contains("cake-maker"),
            "{error}"
        );
        assert!(error.contains("from_the_future"), "{error}");
    }

    /// Attaching with the creator's copy mode is fine; a different mode is refused, naming the
    /// branch, both modes, and the workload that created it.
    #[test]
    fn attach_copy_mode_must_match_the_creator() {
        let existing = pg_branch(
            "b",
            "orders-pg-a1b2c3",
            "cake-maker",
            SqlBranchCopyMode::Schema,
        );
        let same = pg_branch("b", "orders-pg-a1b2c3", "orders", SqlBranchCopyMode::Schema);
        assert!(check_attach_copy_mode(&existing, &same.spec).is_ok());

        let other = pg_branch("b", "orders-pg-a1b2c3", "orders", SqlBranchCopyMode::Empty);
        let error = check_attach_copy_mode(&existing, &other.spec).expect_err("mismatch");
        let OperatorApiError::BranchCopyModeMismatch {
            branch_id,
            existing_mode,
            requested_mode,
            creator,
        } = error
        else {
            panic!("unexpected error {error:?}");
        };
        assert_eq!(branch_id, "orders-pg-a1b2c3");
        assert_eq!(existing_mode, "schema");
        assert_eq!(requested_mode, "empty");
        assert_eq!(creator, "cake-maker");
    }

    /// Two branches under one key copied from the same database warn on the session that owns
    /// the second one, naming both branches, the other branch's creator, and the database.
    /// Branches with distinct sources, or without a recorded one, stay quiet.
    #[test]
    fn same_source_is_warned_once_per_pair_and_only_for_own_branches() {
        let orders = with_source(
            pg_branch(
                "orders",
                "orders-pg-a1b2c3",
                "cake-maker",
                SqlBranchCopyMode::Empty,
            ),
            "orders-db.bakery.svc",
            "orders",
        );
        let main = with_source(
            pg_branch("main", "pg-main-a1b2c3", "shop", SqlBranchCopyMode::Empty),
            "orders-db.bakery.svc",
            "orders",
        );
        let users = with_source(
            pg_branch("users", "users-pg-a1b2c3", "shop", SqlBranchCopyMode::Empty),
            "users-db.bakery.svc",
            "users",
        );
        let unresolved = pg_branch("old", "old-a1b2c3", "shop", SqlBranchCopyMode::Empty);
        let siblings = vec![orders.clone(), main.clone(), users.clone(), unresolved];

        let warnings = same_source_warnings([&main, &users], &siblings);
        let [warning] = warnings.as_slice() else {
            panic!("one shared source gives one warning, got {warnings:?}");
        };
        assert!(
            warning.starts_with(
                "branch `pg-main-a1b2c3` resolved to the same database as `orders-pg-a1b2c3` \
                 (cake-maker): orders-db.bakery.svc:5432/orders"
            ),
            "{warning}"
        );
        assert!(warning.contains("give both SplitConfig entries the same id"));

        assert!(same_source_warnings([&users], &siblings).is_empty());
    }
}
