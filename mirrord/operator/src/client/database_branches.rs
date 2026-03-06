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
        ConnectionSource, ConnectionSourceType, DatabaseBranchConfig, DatabaseBranchesConfig,
        TargetEnviromentVariableSource,
    },
    target::{Target, TargetDisplay},
};
use mirrord_kube::error::KubeApiError;
use mirrord_progress::Progress;
use tracing::Level;
use uuid::Uuid;

use crate::{
    client::error::{OperatorApiError, OperatorOperation},
    crd::db_branching::{
        branch_database::{
            BranchCopyConfig, BranchDatabase, BranchDatabaseSpec, DatabaseDialect, DialectOptions,
        },
        core::{
            BranchDatabasePhase, ConnectionParamsSpec, ConnectionSource as CrdConnectionSource,
            IamAuthConfig as CrdIamAuthConfig,
        },
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

/// List reusable branch databases.
///
/// A branch is considered reusable if it has a user-specified unique ID and is in the "Ready"
/// phase.
pub async fn list_reusable_branches<P: Progress>(
    api: &Api<BranchDatabase>,
    params: &HashMap<BranchDatabaseId, BranchParams>,
    progress: &P,
) -> Result<HashMap<BranchDatabaseId, BranchDatabase>, OperatorApiError> {
    let specified_ids = params
        .iter()
        .filter(|&(id, _)| matches!(id, BranchDatabaseId::Specified(_)))
        .map(|(id, _)| id.as_ref())
        .collect::<Vec<_>>();
    let label_selector = if specified_ids.is_empty() {
        return Ok(HashMap::new());
    } else {
        Some(format!(
            "{} in ({})",
            labels::MIRRORD_BRANCH_ID_LABEL,
            specified_ids.join(",")
        ))
    };

    let mut subtask = progress.subtask("listing reusable branch databases");

    let list_params = ListParams {
        label_selector,
        ..Default::default()
    };
    let reusable_branches = api
        .list(&list_params)
        .await
        .map_err(|e| OperatorApiError::KubeError {
            error: e,
            operation: OperatorOperation::DbBranching,
        })?
        .into_iter()
        .filter(|db| {
            if let Some(status) = &db.status {
                status.phase == BranchDatabasePhase::Ready
            } else {
                false
            }
        })
        .map(|db| (db.spec.id.clone().into(), db))
        .collect::<HashMap<_, _>>();

    subtask.success(Some(&format!(
        "{} reusable branches found",
        reusable_branches.len()
    )));
    Ok(reusable_branches)
}

pub struct DatabaseBranchParams {
    pub branches: HashMap<BranchDatabaseId, BranchParams>,
}

impl DatabaseBranchParams {
    /// Create branch database parameters from user config.
    ///
    /// We generate unique database IDs unless the user explicitly specifies them.
    pub fn new(config: &DatabaseBranchesConfig, target: &Target) -> Self {
        let mut branches = HashMap::new();
        for branch_db_config in config.0.iter() {
            match branch_db_config {
                DatabaseBranchConfig::Mongodb(mongodb_config) => {
                    let id = match mongodb_config.base.id.clone() {
                        Some(id) => BranchDatabaseId::specified(id),
                        None => BranchDatabaseId::generate_new(),
                    };
                    let params = BranchParams::from_mongodb(id.as_ref(), mongodb_config, target);
                    branches.insert(id, params);
                }
                DatabaseBranchConfig::Mysql(mysql_config) => {
                    let id = match mysql_config.base.id.clone() {
                        Some(id) => BranchDatabaseId::specified(id),
                        None => BranchDatabaseId::generate_new(),
                    };
                    let params = BranchParams::from_mysql(id.as_ref(), mysql_config, target);
                    branches.insert(id, params);
                }
                DatabaseBranchConfig::Pg(pg_config) => {
                    let id = match pg_config.base.id.clone() {
                        Some(id) => BranchDatabaseId::specified(id),
                        None => BranchDatabaseId::generate_new(),
                    };
                    let params = BranchParams::from_pg(id.as_ref(), pg_config, target);
                    branches.insert(id, params);
                }
                DatabaseBranchConfig::Mssql(mssql_config) => {
                    let id = match mssql_config.base.id.clone() {
                        Some(id) => BranchDatabaseId::specified(id),
                        None => BranchDatabaseId::generate_new(),
                    };
                    let params = BranchParams::from_mssql(id.as_ref(), mssql_config, target);
                    branches.insert(id, params);
                }
                DatabaseBranchConfig::Redis(_) => {}
            };
        }
        Self { branches }
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
    ) -> Self {
        let name_prefix = format!("{}-pg-branch-", target.name());
        let connection_source = convert_connection_source(&config.base.connection);

        let iam_auth: Option<CrdIamAuthConfig> = config.iam_auth.as_ref().map(Into::into);
        tracing::debug!(?iam_auth, "Converted IAM auth for CRD");

        let dialect_options = iam_auth.map(|auth| DialectOptions {
            iam_auth: Some(auth),
        });

        let spec = BranchDatabaseSpec {
            id: id.to_string(),
            dialect: DatabaseDialect::Postgres,
            database_name: config.base.name.clone(),
            connection_source,
            target: target.clone(),
            ttl_secs: config.base.ttl_secs,
            version: config.base.version.clone(),
            copy: BranchCopyConfig::from(config.copy.clone()),
            dialect_options,
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
    ) -> Self {
        let name_prefix = format!("{}-mysql-branch-", target.name());
        let connection_source = convert_connection_source(&config.base.connection);
        let spec = BranchDatabaseSpec {
            id: id.to_string(),
            dialect: DatabaseDialect::Mysql,
            database_name: config.base.name.clone(),
            connection_source,
            target: target.clone(),
            ttl_secs: config.base.ttl_secs,
            version: config.base.version.clone(),
            copy: BranchCopyConfig::from(config.copy.clone()),
            dialect_options: None,
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
    ) -> Self {
        let name_prefix = format!("{}-mongodb-branch-", target.name());
        let connection_source = convert_connection_source(&config.base.connection);
        let spec = BranchDatabaseSpec {
            id: id.to_string(),
            dialect: DatabaseDialect::Mongodb,
            database_name: config.base.name.clone(),
            connection_source,
            target: target.clone(),
            ttl_secs: config.base.ttl_secs,
            version: config.base.version.clone(),
            copy: BranchCopyConfig::from(config.copy.clone()),
            dialect_options: None,
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
    ) -> Self {
        let name_prefix = format!("{}-mssql-branch-", target.name());
        let connection_source = convert_connection_source(&config.base.connection);
        let spec = BranchDatabaseSpec {
            id: id.to_string(),
            dialect: DatabaseDialect::Mssql,
            database_name: config.base.name.clone(),
            connection_source,
            target: target.clone(),
            ttl_secs: config.base.ttl_secs,
            version: config.base.version.clone(),
            copy: BranchCopyConfig::from(config.copy.clone()),
            dialect_options: None,
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
