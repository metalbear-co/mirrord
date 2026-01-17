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
        ConnectionSource, DatabaseBranchConfig, DatabaseBranchesConfig, MysqlBranchConfig,
        MongodbBranchConfig, PgBranchConfig, TargetEnviromentVariableSource,
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
        mongodb_branching::{
            BranchDatabasePhase as BranchDatabasePhaseMongodb,
            ConnectionSource as CrdConnectionSourceMongodb,
            ConnectionSourceKind as CrdConnectionSourceKindMongodb, MongodbBranchDatabase,
            MongodbBranchDatabaseSpec,
        },
        mysql_branching::{
            BranchDatabasePhase as BranchDatabasePhaseMysql,
            ConnectionSource as CrdConnectionSourceMysql,
            ConnectionSourceKind as CrdConnectionSourceKindMysql, MysqlBranchDatabase,
            MysqlBranchDatabaseSpec,
        },
        pg_branching::{
            BranchDatabasePhase as BranchDatabasePhasePg,
            ConnectionSource as CrdConnectionSourcePg, IamAuthConfig as CrdIamAuthConfig,
            PgBranchDatabase, PgBranchDatabaseSpec,
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

    // Wait for either Ready or Failed phase
    let ready_or_failed = branch_names
        .iter()
        .map(|name| {
            await_condition(api.clone(), name, |db: Option<&MysqlBranchDatabase>| {
                db.and_then(|db| {
                    db.status.as_ref().map(|status| {
                        status.phase == BranchDatabasePhaseMysql::Ready
                            || status.phase == BranchDatabasePhaseMysql::Failed
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
            operation: OperatorOperation::MysqlBranching,
        })?;

    // Check if any branch failed
    for result in results {
        let Ok(Some(db)) = result else {
            continue;
        };
        if let Some(status) = &db.status
            && status.phase == BranchDatabasePhaseMysql::Failed
        {
            let error_msg = status
                .error
                .clone()
                .unwrap_or_else(|| "Branch database creation failed".to_string());
            return Err(OperatorApiError::BranchCreationFailed {
                operation: OperatorOperation::MysqlBranching,
                message: error_msg,
            });
        }
    }

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

    // Wait for either Ready or Failed phase
    let ready_or_failed = branch_names
        .iter()
        .map(|name| {
            await_condition(api.clone(), name, |db: Option<&PgBranchDatabase>| {
                db.and_then(|db| {
                    db.status.as_ref().map(|status| {
                        status.phase == BranchDatabasePhasePg::Ready
                            || status.phase == BranchDatabasePhasePg::Failed
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
            operation: OperatorOperation::PgBranching,
        })?;

    // Check if any branch failed
    for result in results {
        let Ok(Some(db)) = result else {
            continue;
        };
        if let Some(status) = &db.status
            && status.phase == BranchDatabasePhasePg::Failed
        {
            let error_msg = status
                .error
                .clone()
                .unwrap_or_else(|| "Branch database creation failed".to_string());
            return Err(OperatorApiError::BranchCreationFailed {
                operation: OperatorOperation::PgBranching,
                message: error_msg,
            });
        }
    }

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

/// Create MongoDB branch databases and wait for their readiness.
///
/// Timeout after the duration specified by `timeout`.
#[tracing::instrument(level = Level::TRACE, skip_all, err, ret)]
pub(crate) async fn create_mongodb_branches<P: Progress>(
    api: &Api<MongodbBranchDatabase>,
    params: HashMap<BranchDatabaseId, MongodbBranchParams>,
    timeout: Duration,
    progress: &P,
) -> Result<HashMap<BranchDatabaseId, MongodbBranchDatabase>, OperatorApiError> {
    if params.is_empty() {
        return Ok(HashMap::new());
    }

    let mut subtask = progress.subtask("creating new MongoDB branch databases");
    let mut created_branches = HashMap::new();

    for (id, params) in params {
        let name_prefix = params.name_prefix;
        let branch = MongodbBranchDatabase {
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
                    operation: OperatorOperation::MongodbBranching,
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
            await_condition(api.clone(), name, |db: Option<&MongodbBranchDatabase>| {
                db.and_then(|db| {
                    db.status
                        .as_ref()
                        .map(|status| status.phase == BranchDatabasePhaseMongodb::Ready)
                })
                .unwrap_or(false)
            })
        })
        .collect::<Vec<_>>();

    subtask.info("waiting for readiness");
    tokio::time::timeout(timeout, futures::future::join_all(ready))
        .await
        .map_err(|_| OperatorApiError::OperationTimeout {
            operation: OperatorOperation::MongodbBranching,
        })?;
    subtask.success(Some("new MongoDB branch databases ready"));

    Ok(created_branches)
}

/// Given parameters of all MongoDB branch databases needed for a session, list reusable ones.
///
/// A MongoDB branch is considered reusable if
/// 1. it has a user specified unique ID, and
/// 2. it is in the "Ready" phase.
pub(crate) async fn list_reusable_mongodb_branches<P: Progress>(
    api: &Api<MongodbBranchDatabase>,
    params: &HashMap<BranchDatabaseId, MongodbBranchParams>,
    progress: &P,
) -> Result<HashMap<BranchDatabaseId, MongodbBranchDatabase>, OperatorApiError> {
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
            labels::MIRRORD_MONGODB_BRANCH_ID_LABEL,
            specified_ids.join(",")
        ))
    };

    let mut subtask = progress.subtask("listing reusable MongoDB branch databases");

    let list_params = ListParams {
        label_selector,
        ..Default::default()
    };
    let reusable_mongodb_branches = api
        .list(&list_params)
        .await
        .map_err(|e| OperatorApiError::KubeError {
            error: e,
            operation: OperatorOperation::MongodbBranching,
        })?
        .into_iter()
        .filter(|db| {
            if let Some(status) = &db.status {
                status.phase == BranchDatabasePhaseMongodb::Ready
            } else {
                false
            }
        })
        .map(|db| (db.spec.id.clone().into(), db))
        .collect::<HashMap<_, _>>();

    subtask.success(Some(&format!(
        "{} reusable MongoDB branches found",
        reusable_mongodb_branches.len()
    )));
    Ok(reusable_mongodb_branches)
}

pub(crate) struct DatabaseBranchParams {
    pub(crate) mongodb: HashMap<BranchDatabaseId, MongodbBranchParams>,
    pub(crate) mysql: HashMap<BranchDatabaseId, MysqlBranchParams>,
    pub(crate) pg: HashMap<BranchDatabaseId, PgBranchParams>,
}

impl DatabaseBranchParams {
    /// Create branch database parameters.
    ///
    /// We generate unique database IDs unless the user explicitly specifies them.
    pub(crate) fn new(config: &DatabaseBranchesConfig, target: &Target) -> Self {
        let mut mongodb = HashMap::new();
        let mut mysql = HashMap::new();
        let mut pg = HashMap::new();
        for branch_db_config in config.0.iter() {
            match branch_db_config {
                DatabaseBranchConfig::Mongodb(mongodb_config) => {
                    let id = if let Some(id) = mongodb_config.base.id.clone() {
                        BranchDatabaseId::specified(id)
                    } else {
                        BranchDatabaseId::generate_new()
                    };
                    let params = MongodbBranchParams::new(id.as_ref(), mongodb_config, target);
                    mongodb.insert(id, params);
                }
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
                DatabaseBranchConfig::Redis(_) => {}
            };
        }
        Self { mongodb, mysql, pg }
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
                TargetEnviromentVariableSource::Env {
                    container,
                    variable,
                } => CrdConnectionSourceMysql::Url(CrdConnectionSourceKindMysql::Env {
                    container: container.clone(),
                    variable: variable.clone(),
                }),
                TargetEnviromentVariableSource::EnvFrom {
                    container,
                    variable,
                } => CrdConnectionSourceMysql::Url(CrdConnectionSourceKindMysql::EnvFrom {
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
            ConnectionSource::Url(kind) => CrdConnectionSourcePg::Url(kind.into()),
        };

        // Convert IAM auth config if present
        let iam_auth: Option<CrdIamAuthConfig> = config.iam_auth.as_ref().map(Into::into);
        tracing::debug!(?iam_auth, "Converted IAM auth for CRD");
        let spec = PgBranchDatabaseSpec {
            id: id.to_string(),
            database_name: config.base.name.clone(),
            connection_source,
            target: target.clone(),
            ttl_secs: config.base.ttl_secs,
            postgres_version: config.base.version.clone(),
            copy: config.copy.clone().into(),
            iam_auth,
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

#[derive(Debug, Clone)]
pub(crate) struct MongodbBranchParams {
    pub(crate) name_prefix: String,
    pub(crate) labels: BTreeMap<String, String>,
    pub(crate) spec: MongodbBranchDatabaseSpec,
}

impl MongodbBranchParams {
    pub(crate) fn new(id: &str, config: &MongodbBranchConfig, target: &Target) -> Self {
        let name_prefix = format!("{}-mongodb-branch-", target.name());
        let connection_source = match &config.base.connection {
            ConnectionSource::Url(kind) => match kind {
                ConnectionSourceKind::Env {
                    container,
                    variable,
                } => CrdConnectionSourceMongodb::Url(CrdConnectionSourceKindMongodb::Env {
                    container: container.clone(),
                    variable: variable.clone(),
                }),
                ConnectionSourceKind::EnvFrom {
                    container,
                    variable,
                } => CrdConnectionSourceMongodb::Url(CrdConnectionSourceKindMongodb::EnvFrom {
                    container: container.clone(),
                    variable: variable.clone(),
                }),
            },
        };
        let spec = MongodbBranchDatabaseSpec {
            id: id.to_string(),
            database_name: config.base.name.clone(),
            connection_source,
            target: target.clone(),
            ttl_secs: config.base.ttl_secs,
            mongodb_version: config.base.version.clone(),
            copy: config.copy.clone().into(),
        };
        let labels = BTreeMap::from([(
            labels::MIRRORD_MONGODB_BRANCH_ID_LABEL.to_string(),
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
    pub(crate) const MIRRORD_MONGODB_BRANCH_ID_LABEL: &str = "mirrord-mongodb-branch-id";
    pub(crate) const MIRRORD_MYSQL_BRANCH_ID_LABEL: &str = "mirrord-mysql-branch-id";
    pub(crate) const MIRRORD_PG_BRANCH_ID_LABEL: &str = "mirrord-pg-branch-id";
}
