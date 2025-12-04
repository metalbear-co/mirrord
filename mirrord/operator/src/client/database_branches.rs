use std::{
    collections::{BTreeMap, HashMap},
    time::Duration,
};

use kube::{
    Api, Resource,
    api::{ListParams, ObjectMeta},
    runtime::wait::await_condition,
};
use mirrord_config::{
    feature::database_branches::{
        ConnectionSource, ConnectionSourceKind, DatabaseBranchConfig, DatabaseBranchesConfig,
        MysqlBranchConfig, PgBranchConfig,
    },
    target::{Target, TargetDisplay},
};
use mirrord_kube::error::KubeApiError;
use mirrord_progress::Progress;
use tracing::Level;
use uuid::Uuid;

use crate::{
    client::error::{OperatorApiError, OperatorOperation},
    crd::{
        mysql_branching::{
            BranchDatabasePhase as BranchDatabasePhaseMysql,
            ConnectionSource as CrdConnectionSourceMysql,
            ConnectionSourceKind as CrdConnectionSourceKindMysql, MysqlBranchDatabase,
            MysqlBranchDatabaseSpec,
        },
        pg_branching::{
            BranchDatabasePhase as BranchDatabasePhasePg,
            ConnectionSource as CrdConnectionSourcePg,
            ConnectionSourceKind as CrdConnectionSourceKindPg, PgBranchDatabase,
            PgBranchDatabaseSpec,
        },
    },
};

/// Create MySQL branch databases and wait for their readiness.
///
/// Timeout after the duration specified by `timeout`.
#[tracing::instrument(level = Level::TRACE, skip_all, err, ret)]
pub(crate) async fn create_mysql_branches<P: Progress>(
    api: &Api<MysqlBranchDatabase>,
    params: HashMap<BranchDatabaseId, MysqlBranchParams>,
    timeout: Duration,
    progress: &P,
) -> Result<HashMap<BranchDatabaseId, MysqlBranchDatabase>, OperatorApiError> {
    if params.is_empty() {
        return Ok(HashMap::new());
    }

    let mut subtask = progress.subtask("creating new MySQL branch databases");
    let mut created_branches = HashMap::new();

    for (id, params) in params {
        let name_prefix = params.name_prefix;
        let branch = MysqlBranchDatabase {
            metadata: ObjectMeta {
                generate_name: Some(name_prefix),
                labels: Some(params.labels),
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
                    operation: OperatorOperation::MysqlBranching,
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

    let ready = branch_names
        .iter()
        .map(|name| {
            await_condition(api.clone(), name, |db: Option<&MysqlBranchDatabase>| {
                db.and_then(|db| {
                    db.status
                        .as_ref()
                        .map(|status| status.phase == BranchDatabasePhaseMysql::Ready)
                })
                .unwrap_or(false)
            })
        })
        .collect::<Vec<_>>();

    subtask.info("waiting for readiness");
    tokio::time::timeout(timeout, futures::future::join_all(ready))
        .await
        .map_err(|_| OperatorApiError::OperationTimeout {
            operation: OperatorOperation::MysqlBranching,
        })?;
    subtask.success(Some("new MySQL branch databases ready"));

    Ok(created_branches)
}

/// Given parameters of all MySQL branch databases needed for a session, list reusable ones.
///
/// A MySQL branch is considered reusable if
/// 1. it has a user specified unique ID, and
/// 2. it is in the "Ready" phase.
pub(crate) async fn list_reusable_mysql_branches<P: Progress>(
    api: &Api<MysqlBranchDatabase>,
    params: &HashMap<BranchDatabaseId, MysqlBranchParams>,
    progress: &P,
) -> Result<HashMap<BranchDatabaseId, MysqlBranchDatabase>, OperatorApiError> {
    let specified_ids = params
        .iter()
        .filter(|&(id, _)| matches!(id, BranchDatabaseId::Specified(_)))
        .map(|(id, _)| id.as_ref())
        .collect::<Vec<_>>();
    let label_selector = if specified_ids.is_empty() {
        // no branch is reusable as there is no user specified ID.
        return Ok(HashMap::new());
    } else {
        Some(format!(
            "{} in ({})",
            labels::MIRRORD_MYSQL_BRANCH_ID_LABEL,
            specified_ids.join(",")
        ))
    };

    let mut subtask = progress.subtask("listing reusable MySQL branch databases");

    let list_params = ListParams {
        label_selector,
        ..Default::default()
    };
    let reusable_mysql_branches = api
        .list(&list_params)
        .await
        .map_err(|e| OperatorApiError::KubeError {
            error: e,
            operation: OperatorOperation::MysqlBranching,
        })?
        .into_iter()
        .filter(|db| {
            if let Some(status) = &db.status {
                status.phase == BranchDatabasePhaseMysql::Ready
            } else {
                false
            }
        })
        .map(|db| (db.spec.id.clone().into(), db))
        .collect::<HashMap<_, _>>();

    subtask.success(Some(&format!(
        "{} reusable MySQL branches found",
        reusable_mysql_branches.len()
    )));
    Ok(reusable_mysql_branches)
}

/// Create PostgreSQL branch databases and wait for their readiness.
///
/// Timeout after the duration specified by `timeout`.
#[tracing::instrument(level = Level::TRACE, skip_all, err, ret)]
pub(crate) async fn create_pg_branches<P: Progress>(
    api: &Api<PgBranchDatabase>,
    params: HashMap<BranchDatabaseId, PgBranchParams>,
    timeout: Duration,
    progress: &P,
) -> Result<HashMap<BranchDatabaseId, PgBranchDatabase>, OperatorApiError> {
    if params.is_empty() {
        return Ok(HashMap::new());
    }

    let mut subtask = progress.subtask("creating new PostgreSQL branch databases");
    let mut created_branches = HashMap::new();

    for (id, params) in params {
        let name_prefix = params.name_prefix;
        let branch = PgBranchDatabase {
            metadata: ObjectMeta {
                generate_name: Some(name_prefix),
                labels: Some(params.labels),
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
                    operation: OperatorOperation::PgBranching,
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

    let ready = branch_names
        .iter()
        .map(|name| {
            await_condition(api.clone(), name, |db: Option<&PgBranchDatabase>| {
                db.and_then(|db| {
                    db.status
                        .as_ref()
                        .map(|status| status.phase == BranchDatabasePhasePg::Ready)
                })
                .unwrap_or(false)
            })
        })
        .collect::<Vec<_>>();

    subtask.info("waiting for readiness");
    tokio::time::timeout(timeout, futures::future::join_all(ready))
        .await
        .map_err(|_| OperatorApiError::OperationTimeout {
            operation: OperatorOperation::PgBranching,
        })?;
    subtask.success(Some("new PostgreSQL branch databases ready"));

    Ok(created_branches)
}
/// Given parameters of all PostgreSQL branch databases needed for a session, list reusable ones.
///
/// A PostgreSQL branch is considered reusable if
/// 1. it has a user specified unique ID, and
/// 2. it is in the "Ready" phase.
pub(crate) async fn list_reusable_pg_branches<P: Progress>(
    api: &Api<PgBranchDatabase>,
    params: &HashMap<BranchDatabaseId, PgBranchParams>,
    progress: &P,
) -> Result<HashMap<BranchDatabaseId, PgBranchDatabase>, OperatorApiError> {
    let specified_ids = params
        .iter()
        .filter(|&(id, _)| matches!(id, BranchDatabaseId::Specified(_)))
        .map(|(id, _)| id.as_ref())
        .collect::<Vec<_>>();
    let label_selector = if specified_ids.is_empty() {
        // no branch is reusable as there is no user specified ID.
        return Ok(HashMap::new());
    } else {
        Some(format!(
            "{} in ({})",
            labels::MIRRORD_PG_BRANCH_ID_LABEL,
            specified_ids.join(",")
        ))
    };

    let mut subtask = progress.subtask("listing reusable PostgreSQL branch databases");

    let list_params = ListParams {
        label_selector,
        ..Default::default()
    };
    let reusable_pg_branches = api
        .list(&list_params)
        .await
        .map_err(|e| OperatorApiError::KubeError {
            error: e,
            operation: OperatorOperation::PgBranching,
        })?
        .into_iter()
        .filter(|db| {
            if let Some(status) = &db.status {
                status.phase == BranchDatabasePhasePg::Ready
            } else {
                false
            }
        })
        .map(|db| (db.spec.id.clone().into(), db))
        .collect::<HashMap<_, _>>();

    subtask.success(Some(&format!(
        "{} reusable PostgreSQL branches found",
        reusable_pg_branches.len()
    )));
    Ok(reusable_pg_branches)
}

pub(crate) struct DatabaseBranchParams {
    pub(crate) mysql: HashMap<BranchDatabaseId, MysqlBranchParams>,
    pub(crate) pg: HashMap<BranchDatabaseId, PgBranchParams>,
}

impl DatabaseBranchParams {
    /// Create branch database parameters.
    ///
    /// We generate unique database IDs unless the user explicitly specifies them.
    pub(crate) fn new(config: &DatabaseBranchesConfig, target: &Target) -> Self {
        let mut mysql = HashMap::new();
        let mut pg = HashMap::new();
        for branch_db_config in config.0.iter() {
            match branch_db_config {
                DatabaseBranchConfig::Mysql(mysql_config) => {
                    let id = if let Some(id) = mysql_config.base.id.clone() {
                        BranchDatabaseId::specified(id)
                    } else {
                        BranchDatabaseId::generate_new()
                    };
                    let params = MysqlBranchParams::new(id.as_ref(), mysql_config, target);
                    mysql.insert(id, params);
                }
                DatabaseBranchConfig::Pg(pg_config) => {
                    let id = if let Some(id) = pg_config.base.id.clone() {
                        BranchDatabaseId::specified(id)
                    } else {
                        BranchDatabaseId::generate_new()
                    };
                    let params = PgBranchParams::new(id.as_ref(), pg_config, target);
                    pg.insert(id, params);
                }
            };
        }
        Self { mysql, pg }
    }
}

/// Branch database IDs are either generated unique IDs or given directly by the user.
///
/// This ID is used for selecting reusable branch database and should not be confused with
/// Kubernetes resource uid.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum BranchDatabaseId {
    Specified(String),
    Generated(String),
}

impl BranchDatabaseId {
    /// Use a specified ID directly.
    pub(crate) fn specified(value: String) -> Self {
        Self::Specified(value)
    }

    /// Generate a new UUID.
    pub(crate) fn generate_new() -> Self {
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

#[derive(Debug, Clone)]
pub(crate) struct MysqlBranchParams {
    pub(crate) name_prefix: String,
    pub(crate) labels: BTreeMap<String, String>,
    pub(crate) spec: MysqlBranchDatabaseSpec,
}

impl MysqlBranchParams {
    pub(crate) fn new(id: &str, config: &MysqlBranchConfig, target: &Target) -> Self {
        let name_prefix = format!("{}-mysql-branch-", target.name());
        let connection_source = match &config.base.connection {
            ConnectionSource::Url(kind) => match kind {
                ConnectionSourceKind::Env {
                    container,
                    variable,
                } => CrdConnectionSourceMysql::Url(CrdConnectionSourceKindMysql::Env {
                    container: container.clone(),
                    variable: variable.clone(),
                }),
            },
        };
        let spec = MysqlBranchDatabaseSpec {
            id: id.to_string(),
            database_name: config.base.name.clone(),
            connection_source,
            target: target.clone(),
            ttl_secs: config.base.ttl_secs,
            mysql_version: config.base.version.clone(),
            copy: config.copy.clone().into(),
        };
        let labels = BTreeMap::from([(
            labels::MIRRORD_MYSQL_BRANCH_ID_LABEL.to_string(),
            id.to_string(),
        )]);
        Self {
            name_prefix,
            labels,
            spec,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PgBranchParams {
    pub(crate) name_prefix: String,
    pub(crate) labels: BTreeMap<String, String>,
    pub(crate) spec: PgBranchDatabaseSpec,
}

impl PgBranchParams {
    pub(crate) fn new(id: &str, config: &PgBranchConfig, target: &Target) -> Self {
        let name_prefix = format!("{}-pg-branch-", target.name());
        let connection_source = match &config.base.connection {
            ConnectionSource::Url(kind) => match kind {
                ConnectionSourceKind::Env {
                    container,
                    variable,
                } => CrdConnectionSourcePg::Url(CrdConnectionSourceKindPg::Env {
                    container: container.clone(),
                    variable: variable.clone(),
                }),
            },
        };
        let spec = PgBranchDatabaseSpec {
            id: id.to_string(),
            database_name: config.base.name.clone(),
            connection_source,
            target: target.clone(),
            ttl_secs: config.base.ttl_secs,
            postgres_version: config.base.version.clone(),
            copy: config.copy.clone().into(),
        };
        let labels = BTreeMap::from([(
            labels::MIRRORD_PG_BRANCH_ID_LABEL.to_string(),
            id.to_string(),
        )]);
        Self {
            name_prefix,
            labels,
            spec,
        }
    }
}
pub(crate) mod labels {
    pub(crate) const MIRRORD_MYSQL_BRANCH_ID_LABEL: &str = "mirrord-mysql-branch-id";
    pub(crate) const MIRRORD_PG_BRANCH_ID_LABEL: &str = "mirrord-pg-branch-id";
}
