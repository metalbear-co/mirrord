use std::{
    collections::{BTreeMap, HashMap},
    time::Duration,
};

use kube::{
    Api, Client, Resource,
    api::{ListParams, ObjectMeta},
    runtime::wait::await_condition,
};
use mirrord_config::{
    feature::database_branches::{
        ConnectionSource, ConnectionSourceType, DatabaseBranchConfig, DatabaseBranchesConfig,
        TargetEnviromentVariableSource,
    },
    target::{Target, TargetDisplay},
};
use mirrord_kube::{api::runtime::RuntimeDataProvider, error::KubeApiError};
use mirrord_progress::Progress;
use tracing::Level;
use uuid::Uuid;

use crate::{
    client::error::{OperatorApiError, OperatorOperation},
    crd::{
        db_branching::{
            branch_database::{
                BranchDatabase, BranchDatabaseSpec, DialectConfig, MongodbOptions, MssqlOptions,
                MysqlOptions, PostgresOptions, SqlBranchCopyConfig,
            },
            core::{
                BranchDatabasePhase, ConnectionParamsSpec, ConnectionSource as CrdConnectionSource,
                IamAuthConfig as CrdIamAuthConfig,
            },
        },
        session::SessionTarget,
    },
};

/// Create branch databases and wait for their readiness.
///
/// Timeout after the duration specified by `timeout`.
#[tracing::instrument(level = Level::TRACE, skip_all, err, ret)]
pub async fn create_branches<P: Progress>(
    api: &Api<BranchDatabase>,
    params: HashMap<BranchDatabaseId, BranchParams>,
    timeout: Duration,
    progress: &P,
) -> Result<HashMap<BranchDatabaseId, BranchDatabase>, OperatorApiError> {
    if params.is_empty() {
        return Ok(HashMap::new());
    }

    let mut subtask = progress.subtask("creating new branch databases");
    let mut created_branches = HashMap::new();

    for (id, params) in params {
        let name_prefix = params.name_prefix;
        let annotations = if params.annotations.is_empty() {
            None
        } else {
            Some(params.annotations)
        };
        let branch = BranchDatabase {
            metadata: ObjectMeta {
                generate_name: Some(name_prefix),
                labels: Some(params.labels),
                annotations,
                ..Default::default()
            },
            spec: params.spec,
            status: None,
        };

        match api.create(&kube::api::PostParams::default(), &branch).await {
            Ok(branch) => created_branches.insert(id, branch),
            Err(e) => {
                return Err(OperatorApiError::KubeError {
                    error: e,
                    operation: OperatorOperation::DbBranching,
                });
            }
        };
    }
    subtask.info("databases created");

    let branch_names = created_branches
        .values()
        .map(|branch| {
            branch
                .meta()
                .name
                .clone()
                .ok_or(KubeApiError::missing_field(branch, ".metadata.name"))
        })
        .collect::<Result<Vec<_>, _>>()?;

    let ready_or_failed = branch_names
        .iter()
        .map(|name| {
            await_condition(api.clone(), name, |db: Option<&BranchDatabase>| {
                db.and_then(|db| {
                    db.status.as_ref().map(|status| {
                        status.phase == BranchDatabasePhase::Ready
                            || status.phase == BranchDatabasePhase::Failed
                    })
                })
                .unwrap_or(false)
            })
        })
        .collect::<Vec<_>>();

    subtask.info("waiting for readiness");
    let results = tokio::time::timeout(timeout, futures::future::join_all(ready_or_failed))
        .await
        .map_err(|_| OperatorApiError::OperationTimeout {
            operation: OperatorOperation::DbBranching,
        })?;

    for result in results {
        let Ok(Some(db)) = result else {
            continue;
        };
        if let Some(status) = &db.status
            && status.phase == BranchDatabasePhase::Failed
        {
            let error_msg = status
                .error
                .clone()
                .unwrap_or_else(|| "Branch database creation failed".to_string());
            return Err(OperatorApiError::BranchCreationFailed {
                operation: OperatorOperation::DbBranching,
                message: error_msg,
            });
        }
    }

    subtask.success(Some("new branch databases ready"));

    Ok(created_branches)
}

/// Branches found by ID that can be reused or waited on.
pub struct ExistingBranches {
    /// Branches already in Ready phase, can be used immediately.
    pub ready: HashMap<BranchDatabaseId, BranchDatabase>,
    /// Branches still being created (not Ready, not Failed). The caller should wait
    /// for these instead of creating duplicates.
    pub pending: HashMap<BranchDatabaseId, BranchDatabase>,
}

/// List existing branch databases that match user-specified IDs.
///
/// Returns branches split into two groups:
/// - `ready`: branches in Ready phase that can be reused immediately
/// - `pending`: branches still initializing that the caller should wait for
///
/// Failed branches are ignored so a fresh one can be created.
pub async fn list_existing_branches<P: Progress>(
    api: &Api<BranchDatabase>,
    params: &HashMap<BranchDatabaseId, BranchParams>,
    progress: &P,
) -> Result<ExistingBranches, OperatorApiError> {
    let specified_ids = params
        .iter()
        .filter(|&(id, _)| matches!(id, BranchDatabaseId::Specified(_)))
        .map(|(id, _)| id.as_ref())
        .collect::<Vec<_>>();
    let label_selector = if specified_ids.is_empty() {
        return Ok(ExistingBranches {
            ready: HashMap::new(),
            pending: HashMap::new(),
        });
    } else {
        Some(format!(
            "{} in ({})",
            labels::MIRRORD_BRANCH_ID_LABEL,
            specified_ids.join(",")
        ))
    };

    let mut subtask = progress.subtask("listing existing branch databases");

    let list_params = ListParams {
        label_selector,
        ..Default::default()
    };
    let all_branches: Vec<BranchDatabase> = api
        .list(&list_params)
        .await
        .map_err(|e| OperatorApiError::KubeError {
            error: e,
            operation: OperatorOperation::DbBranching,
        })?
        .into_iter()
        .collect();

    let mut ready = HashMap::new();
    let mut pending = HashMap::new();

    for db in all_branches {
        let id: BranchDatabaseId = db.spec.id.clone().into();
        match db.status.as_ref().map(|s| &s.phase) {
            Some(&BranchDatabasePhase::Ready) => {
                ready.insert(id, db);
            }
            Some(&BranchDatabasePhase::Failed) => {
                // Skip failed branches so a new one will be created
            }
            _ => {
                // Initializing, Pending, or no status yet -- still being created
                pending.insert(id, db);
            }
        }
    }

    subtask.success(Some(&format!(
        "{} ready, {} pending",
        ready.len(),
        pending.len()
    )));
    Ok(ExistingBranches { ready, pending })
}

/// Wait for pending branch databases to become Ready or Failed.
///
/// Returns the branches that reached Ready. Returns an error if any branch failed.
pub async fn wait_for_pending_branches<P: Progress>(
    api: &Api<BranchDatabase>,
    pending: &HashMap<BranchDatabaseId, BranchDatabase>,
    timeout: Duration,
    progress: &P,
) -> Result<HashMap<BranchDatabaseId, BranchDatabase>, OperatorApiError> {
    if pending.is_empty() {
        return Ok(HashMap::new());
    }

    let mut subtask = progress.subtask("waiting for in-progress branch databases");

    let branch_names: Vec<(BranchDatabaseId, String)> = pending
        .iter()
        .filter_map(|(id, db)| {
            db.meta()
                .name
                .clone()
                .map(|name| (id.clone(), name))
        })
        .collect();

    let wait_futures = branch_names
        .iter()
        .map(|(_, name)| {
            await_condition(api.clone(), name, |db: Option<&BranchDatabase>| {
                db.and_then(|db| {
                    db.status.as_ref().map(|status| {
                        status.phase == BranchDatabasePhase::Ready
                            || status.phase == BranchDatabasePhase::Failed
                    })
                })
                .unwrap_or(false)
            })
        })
        .collect::<Vec<_>>();

    subtask.info("waiting for readiness");
    let results = tokio::time::timeout(timeout, futures::future::join_all(wait_futures))
        .await
        .map_err(|_| OperatorApiError::OperationTimeout {
            operation: OperatorOperation::DbBranching,
        })?;

    let mut ready_branches = HashMap::new();
    for (result, (id, _name)) in results.into_iter().zip(branch_names.iter()) {
        let Ok(Some(db)) = result else {
            continue;
        };

        if let Some(status) = &db.status
            && status.phase == BranchDatabasePhase::Failed
        {
            let error_msg = status
                .error
                .clone()
                .unwrap_or_else(|| "Branch database creation failed".to_string());
            return Err(OperatorApiError::BranchCreationFailed {
                operation: OperatorOperation::DbBranching,
                message: error_msg,
            });
        }

        ready_branches.insert(id.clone(), db);
    }

    subtask.success(Some(&format!(
        "{} pending branches now ready",
        ready_branches.len()
    )));
    Ok(ready_branches)
}

/// Resolve a branch database ID from the user config and session key.
///
/// - No `id` in config: uses the session key as the branch ID (enables automatic reuse)
/// - `id` contains `{key}`: substitutes `{key}` with the session key
/// - `id` without `{key}`: uses the custom ID as-is, emits a warning
fn resolve_branch_id<P: Progress>(
    config_id: &Option<String>,
    session_key: &str,
    progress: &P,
) -> BranchDatabaseId {
    match config_id {
        None => BranchDatabaseId::specified(session_key.to_string()),
        Some(id) if id.contains("{key}") => {
            let resolved = id.replace("{key}", session_key);
            BranchDatabaseId::specified(resolved)
        }
        Some(id) => {
            progress.warning(
                "Custom branch ID provided, session key will not be used. \
                 Remove the ID or use {{ key }} to include it.",
            );
            BranchDatabaseId::specified(id.clone())
        }
    }
}

pub struct DatabaseBranchParams {
    pub branches: HashMap<BranchDatabaseId, BranchParams>,
}

impl DatabaseBranchParams {
    /// Create branch database parameters from user config.
    ///
    /// When no branch `id` is provided, the session key is used as the branch ID so that
    /// sessions sharing the same key automatically reuse the same branch. Custom IDs can
    /// reference `{key}` for substitution.
    ///
    /// If the target has no container set, resolves it from the cluster via
    /// [`RuntimeDataProvider`].
    pub async fn new<P: Progress>(
        config: &DatabaseBranchesConfig,
        target: &Target,
        client: &Client,
        namespace: Option<&str>,
        session_key: &str,
        progress: &P,
    ) -> Result<Self, OperatorApiError> {
        let mut target_with_container = target.clone();
        if target_with_container.container().is_none() {
            let runtime_data = target
                .runtime_data(client, namespace)
                .await
                .map_err(OperatorApiError::KubeApi)?;
            target_with_container.set_container(runtime_data.container_name);
        }
        let target_display = target_with_container.to_string();
        let session_target = SessionTarget::from_config(target_with_container)
            .ok_or_else(|| OperatorApiError::TargetResolutionFailed(target_display))?;

        let mut branches = HashMap::new();
        for branch_db_config in config.0.iter() {
            match branch_db_config {
                DatabaseBranchConfig::Mongodb(mongodb_config) => {
                    let id = resolve_branch_id(
                        &mongodb_config.base.id,
                        session_key,
                        progress,
                    );
                    let params = BranchParams::from_mongodb(
                        id.as_ref(),
                        mongodb_config,
                        target,
                        &session_target,
                    );
                    branches.insert(id, params);
                }
                DatabaseBranchConfig::Mysql(mysql_config) => {
                    let id = resolve_branch_id(
                        &mysql_config.base.id,
                        session_key,
                        progress,
                    );
                    let params = BranchParams::from_mysql(
                        id.as_ref(),
                        mysql_config,
                        target,
                        &session_target,
                    );
                    branches.insert(id, params);
                }
                DatabaseBranchConfig::Pg(pg_config) => {
                    let id = resolve_branch_id(
                        &pg_config.base.id,
                        session_key,
                        progress,
                    );
                    let params =
                        BranchParams::from_pg(id.as_ref(), pg_config, target, &session_target);
                    branches.insert(id, params);
                }
                DatabaseBranchConfig::Mssql(mssql_config) => {
                    let id = resolve_branch_id(
                        &mssql_config.base.id,
                        session_key,
                        progress,
                    );
                    let params = BranchParams::from_mssql(
                        id.as_ref(),
                        mssql_config,
                        target,
                        &session_target,
                    );
                    branches.insert(id, params);
                }
                DatabaseBranchConfig::Redis(_) => {}
            };
        }
        Ok(Self { branches })
    }
}

/// Branch database IDs are either generated unique IDs or given directly by the user.
///
/// This ID is used for selecting reusable branch database and should not be confused with
/// Kubernetes resource uid.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BranchDatabaseId {
    Specified(String),
    Generated(String),
}

impl BranchDatabaseId {
    pub fn specified(value: String) -> Self {
        Self::Specified(value)
    }

    pub fn generate_new() -> Self {
        Self::Generated(Uuid::new_v4().to_string())
    }
}

impl From<String> for BranchDatabaseId {
    fn from(value: String) -> Self {
        BranchDatabaseId::Specified(value)
    }
}

impl std::fmt::Display for BranchDatabaseId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BranchDatabaseId::Specified(id) | BranchDatabaseId::Generated(id) => {
                write!(f, "{}", id)
            }
        }
    }
}

impl AsRef<str> for BranchDatabaseId {
    fn as_ref(&self) -> &str {
        match self {
            BranchDatabaseId::Specified(id) | BranchDatabaseId::Generated(id) => id.as_ref(),
        }
    }
}

fn convert_connection_source(source: &ConnectionSource) -> CrdConnectionSource {
    match source {
        ConnectionSource::Url { url } => CrdConnectionSource::Url(Box::new(url.into())),
        ConnectionSource::FlatUrl { source_type, url } => {
            let kind = match source_type {
                Some(ConnectionSourceType::EnvFrom) => TargetEnviromentVariableSource::EnvFrom {
                    container: None,
                    variable: url.clone(),
                },
                // None or Env: default to Env. The operator auto-detects
                // envFrom at resolution time if needed.
                _ => TargetEnviromentVariableSource::Env {
                    container: None,
                    variable: url.clone(),
                },
            };
            CrdConnectionSource::Url(Box::new((&kind).into()))
        }
        ConnectionSource::Params(config) => {
            CrdConnectionSource::Params(Box::new(ConnectionParamsSpec::from(config)))
        }
    }
}

#[derive(Debug, Clone)]
pub struct BranchParams {
    pub name_prefix: String,
    pub labels: BTreeMap<String, String>,
    pub annotations: BTreeMap<String, String>,
    pub spec: BranchDatabaseSpec,
}

impl BranchParams {
    pub fn from_pg(
        id: &str,
        config: &mirrord_config::feature::database_branches::PgBranchConfig,
        target: &Target,
        session_target: &SessionTarget,
    ) -> Self {
        let name_prefix = format!("{}-pg-branch-", target.name());
        let connection_source = convert_connection_source(&config.base.connection);

        let iam_auth: Option<CrdIamAuthConfig> = config.iam_auth.as_ref().map(Into::into);
        tracing::debug!(?iam_auth, "Converted IAM auth for CRD");

        let spec = BranchDatabaseSpec {
            id: id.to_string(),
            database_name: config.base.name.clone(),
            connection_source,
            target: session_target.clone(),
            ttl_secs: config.base.ttl_secs,
            version: config.base.version.clone(),
            dialect: DialectConfig::Postgres(Box::new(PostgresOptions {
                copy: SqlBranchCopyConfig::from(config.copy.clone()),
                iam_auth,
            })),
        };
        let labels =
            BTreeMap::from([(labels::MIRRORD_BRANCH_ID_LABEL.to_string(), id.to_string())]);
        Self {
            name_prefix,
            labels,
            annotations: BTreeMap::new(),
            spec,
        }
    }

    pub fn from_mysql(
        id: &str,
        config: &mirrord_config::feature::database_branches::MysqlBranchConfig,
        target: &Target,
        session_target: &SessionTarget,
    ) -> Self {
        let name_prefix = format!("{}-mysql-branch-", target.name());
        let connection_source = convert_connection_source(&config.base.connection);
        let spec = BranchDatabaseSpec {
            id: id.to_string(),
            database_name: config.base.name.clone(),
            connection_source,
            target: session_target.clone(),
            ttl_secs: config.base.ttl_secs,
            version: config.base.version.clone(),
            dialect: DialectConfig::Mysql(Box::new(MysqlOptions {
                copy: SqlBranchCopyConfig::from(config.copy.clone()),
            })),
        };
        let labels =
            BTreeMap::from([(labels::MIRRORD_BRANCH_ID_LABEL.to_string(), id.to_string())]);
        Self {
            name_prefix,
            labels,
            annotations: BTreeMap::new(),
            spec,
        }
    }

    pub fn from_mongodb(
        id: &str,
        config: &mirrord_config::feature::database_branches::MongodbBranchConfig,
        target: &Target,
        session_target: &SessionTarget,
    ) -> Self {
        let name_prefix = format!("{}-mongodb-branch-", target.name());
        let connection_source = convert_connection_source(&config.base.connection);
        let spec = BranchDatabaseSpec {
            id: id.to_string(),
            database_name: config.base.name.clone(),
            connection_source,
            target: session_target.clone(),
            ttl_secs: config.base.ttl_secs,
            version: config.base.version.clone(),
            dialect: DialectConfig::Mongodb(Box::new(MongodbOptions {
                copy: config.copy.clone().into(),
            })),
        };
        let labels =
            BTreeMap::from([(labels::MIRRORD_BRANCH_ID_LABEL.to_string(), id.to_string())]);
        Self {
            name_prefix,
            labels,
            annotations: BTreeMap::new(),
            spec,
        }
    }

    pub fn from_mssql(
        id: &str,
        config: &mirrord_config::feature::database_branches::MssqlBranchConfig,
        target: &Target,
        session_target: &SessionTarget,
    ) -> Self {
        let name_prefix = format!("{}-mssql-branch-", target.name());
        let connection_source = convert_connection_source(&config.base.connection);
        let spec = BranchDatabaseSpec {
            id: id.to_string(),
            database_name: config.base.name.clone(),
            connection_source,
            target: session_target.clone(),
            ttl_secs: config.base.ttl_secs,
            version: config.base.version.clone(),
            dialect: DialectConfig::Mssql(Box::new(MssqlOptions {
                copy: SqlBranchCopyConfig::from(config.copy.clone()),
            })),
        };
        let labels =
            BTreeMap::from([(labels::MIRRORD_BRANCH_ID_LABEL.to_string(), id.to_string())]);
        Self {
            name_prefix,
            labels,
            annotations: BTreeMap::new(),
            spec,
        }
    }
}

pub mod labels {
    pub const MIRRORD_BRANCH_ID_LABEL: &str = "mirrord-branch-id";
}

pub use crate::crd::TARGET_NAMESPACE_ANNOTATION;
