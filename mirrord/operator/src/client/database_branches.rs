use std::{
    collections::{BTreeMap, HashMap},
    time::Duration,
};

use flate2::{Compression, write::GzEncoder};
use k8s_openapi::ByteString;
use kube::{
    Api, Resource,
    api::{ListParams, ObjectMeta, Patch, PatchParams},
    runtime::wait::await_condition,
};
use mirrord_config::{
    feature::database_branches::{
        ClickhouseBranchConfig, ConnectionSource as ConfigConnectionSource, ConnectionSourceType,
        DatabaseBranchConfig, DatabaseBranchesConfig, DynamodbBranchConfig, MongodbBranchConfig,
        MysqlBranchConfig, ParamSource, PgBranchConfig, RedisBranchConfig, SingleOrVec,
        SpannerBranchConfig, SqlBranchMigrationsConfig, TargetEnvironmentVariableSource,
        redis::RemoteRedisBranchConfig,
    },
    target::{Target, TargetDisplay},
};
use mirrord_kube::error::KubeApiError;
use mirrord_progress::Progress;
use sha2::{Digest, Sha256};
use tracing::Level;
use uuid::Uuid;
use walkdir::WalkDir;

use crate::{
    client::error::{OperatorApiError, OperatorOperation},
    crd::db_branching::{
        branch_database::{
            BranchDatabase, BranchDatabaseSpec, ClickhouseOptions, DynamodbOptions, MigrationsSpec,
            MongodbOptions, MssqlOptions, MysqlOptions, PostgresOptions, RedisOptions,
            SpannerOptions, SqlBranchCopyConfig,
        },
        core::{
            BranchDatabasePhase, ConnectionParamsSpec, ConnectionSource as CrdConnectionSource,
            IamAuthConfig as CrdIamAuthConfig, MigrationPhase,
        },
        mongodb::{MongodbBranchDatabase, MongodbBranchDatabaseSpec},
        mysql::{MysqlBranchDatabase, MysqlBranchDatabaseSpec},
        pg::{PgBranchDatabase, PgBranchDatabaseSpec},
    },
    types::{OPERATOR_ISOLATION_MARKER_ENV, OPERATOR_OWNERSHIP_LABEL},
};

/// Create MySQL branch databases and wait for their readiness.
///
/// Timeout after the duration specified by `timeout`.
#[tracing::instrument(level = Level::TRACE, skip_all, err, ret)]
pub async fn create_mysql_branches<P: Progress>(
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
        let annotations = if params.annotations.is_empty() {
            None
        } else {
            Some(params.annotations)
        };
        let branch = MysqlBranchDatabase {
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
            operation: OperatorOperation::MysqlBranching,
        })?;

    // Check if any branch failed
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
                .unwrap_or_else(|| "Branch database creation failed".to_owned());
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
pub async fn list_reusable_mysql_branches<P: Progress>(
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
            "{} in ({}),{}",
            labels::MIRRORD_MYSQL_BRANCH_ID_LABEL,
            specified_ids.join(","),
            ownership_label_selector(),
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
                status.phase == BranchDatabasePhase::Ready
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
pub async fn create_pg_branches<P: Progress>(
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
        let annotations = if params.annotations.is_empty() {
            None
        } else {
            Some(params.annotations)
        };
        let branch = PgBranchDatabase {
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
            operation: OperatorOperation::PgBranching,
        })?;

    // Check if any branch failed
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
                .unwrap_or_else(|| "Branch database creation failed".to_owned());
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
pub async fn list_reusable_pg_branches<P: Progress>(
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
            "{} in ({}),{}",
            labels::MIRRORD_PG_BRANCH_ID_LABEL,
            specified_ids.join(","),
            ownership_label_selector(),
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
                status.phase == BranchDatabasePhase::Ready
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
pub async fn create_mongodb_branches<P: Progress>(
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
        let annotations = if params.annotations.is_empty() {
            None
        } else {
            Some(params.annotations)
        };
        let branch = MongodbBranchDatabase {
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
                        .map(|status| status.phase == BranchDatabasePhase::Ready)
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
pub async fn list_reusable_mongodb_branches<P: Progress>(
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
            "{} in ({}),{}",
            labels::MIRRORD_MONGODB_BRANCH_ID_LABEL,
            specified_ids.join(","),
            ownership_label_selector(),
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
                status.phase == BranchDatabasePhase::Ready
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

pub struct DatabaseBranchParams {
    pub mongodb: HashMap<BranchDatabaseId, MongodbBranchParams>,
    pub mysql: HashMap<BranchDatabaseId, MysqlBranchParams>,
    pub pg: HashMap<BranchDatabaseId, PgBranchParams>,
}

impl DatabaseBranchParams {
    /// Create branch database parameters.
    ///
    /// We generate unique database IDs unless the user explicitly specifies them.
    pub fn new(config: &DatabaseBranchesConfig, target: &Target) -> Self {
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
                DatabaseBranchConfig::Mssql(_)
                | DatabaseBranchConfig::Redis(_)
                | DatabaseBranchConfig::Dynamodb(_)
                | DatabaseBranchConfig::Spanner(_) => {}
                DatabaseBranchConfig::Clickhouse(_) => {}
            };
        }

        if let Ok(marker) = std::env::var(OPERATOR_ISOLATION_MARKER_ENV) {
            for params in mongodb.values_mut() {
                params
                    .labels
                    .insert(OPERATOR_OWNERSHIP_LABEL.to_owned(), marker.clone());
            }
            for params in mysql.values_mut() {
                params
                    .labels
                    .insert(OPERATOR_OWNERSHIP_LABEL.to_owned(), marker.clone());
            }
            for params in pg.values_mut() {
                params
                    .labels
                    .insert(OPERATOR_OWNERSHIP_LABEL.to_owned(), marker.clone());
            }
        }

        Self { mongodb, mysql, pg }
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
    /// Use a specified ID directly.
    pub fn specified(value: String) -> Self {
        Self::Specified(value)
    }

    /// Generate a new UUID.
    pub fn generate_new() -> Self {
        Self::Generated(Uuid::new_v4().to_string())
    }
}

impl From<String> for BranchDatabaseId {
    fn from(value: String) -> Self {
        BranchDatabaseId::Specified(value)
    }
}
impl From<BranchDatabaseId> for String {
    fn from(value: BranchDatabaseId) -> Self {
        match value {
            BranchDatabaseId::Specified(s) | BranchDatabaseId::Generated(s) => s,
        }
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

/// Extract all literal `value` fields from a CRD connection source, collecting
/// them into `values_out` keyed by variable name. Each extracted value is removed
/// from the source kind so that `replace_values_with_secret_refs` can fill in the
/// Secret reference afterwards.
#[cfg(feature = "client")]
/// Collects literal `value` fields from `ParamSource::Env` entries in the config
pub fn extract_literal_values(
    source: &mut ConfigConnectionSource,
    values_out: &mut std::collections::HashMap<String, String>,
) {
    fn extract_from_param(
        param: &mut ParamSource,
        values_out: &mut std::collections::HashMap<String, String>,
    ) {
        if let ParamSource::Env {
            env_var_name,
            value: value @ Some(_),
        } = param
        {
            values_out.insert(env_var_name.clone(), value.take().unwrap());
        }
    }

    fn extract_from_env_source(
        src: &mut TargetEnvironmentVariableSource,
        values_out: &mut std::collections::HashMap<String, String>,
    ) {
        if let TargetEnvironmentVariableSource::Env {
            variable,
            value: value @ Some(_),
            ..
        } = src
        {
            values_out.insert(variable.clone(), value.take().unwrap());
        }
    }

    match source {
        ConfigConnectionSource::Url { url } => extract_from_env_source(url, values_out),
        ConfigConnectionSource::FlatUrl { .. } => {}
        ConfigConnectionSource::Params(config) => {
            for param in [
                &mut config.params.host,
                &mut config.params.port,
                &mut config.params.user,
                &mut config.params.password,
                &mut config.params.database,
            ]
            .into_iter()
            .flatten()
            .flat_map(|om| om.0.iter_mut())
            {
                extract_from_param(param, values_out);
            }
        }
    }
}

/// Replaces `Env` source kinds whose variable name matches a key in `extracted_keys`
/// with `Secret { name, key }` source kinds. Called after the operator has created
/// the Secret and returned its name.
#[cfg(feature = "client")]
pub fn replace_values_with_secret_refs(
    source: &mut CrdConnectionSource,
    secret_name: &str,
    literal_values: &std::collections::HashMap<String, String>,
) {
    use crate::crd::db_branching::core::ConnectionSourceKind;

    fn replace_kind(
        kind: &mut ConnectionSourceKind,
        secret_name: &str,
        literal_values: &std::collections::HashMap<String, String>,
    ) {
        if let ConnectionSourceKind::Env { variable, .. } = kind
            && literal_values.contains_key(variable.as_str())
        {
            // We reuse the original variable name for both fields: the CLI
            // already stored the value under that name in the Secret (so it's
            // the data key), and that's also the env var the user's app reads.
            *kind = ConnectionSourceKind::Secret {
                name: secret_name.to_owned(),
                key: variable.clone(),
                env_var_name: Some(variable.clone()),
            };
        }
    }

    match source {
        CrdConnectionSource::Url(kinds) => {
            for kind in kinds {
                replace_kind(kind, secret_name, literal_values);
            }
        }
        CrdConnectionSource::Params(params) => {
            for kind in [
                &mut params.host,
                &mut params.port,
                &mut params.user,
                &mut params.password,
                &mut params.database,
            ]
            .into_iter()
            .flatten()
            .flatten()
            {
                replace_kind(kind, secret_name, literal_values);
            }
        }
    }
}

fn convert_connection_source(source: &ConfigConnectionSource) -> CrdConnectionSource {
    match source {
        ConfigConnectionSource::Url { url } => {
            CrdConnectionSource::Url(SingleOrVec::from(vec![url.into()]))
        }
        ConfigConnectionSource::FlatUrl { source_type, url } => {
            let kinds: Vec<_> = url
                .iter()
                .map(|u| {
                    let kind = match source_type {
                        Some(ConnectionSourceType::EnvFrom) => {
                            TargetEnvironmentVariableSource::EnvFrom {
                                container: None,
                                variable: u.clone(),
                            }
                        }
                        _ => TargetEnvironmentVariableSource::Env {
                            container: None,
                            variable: u.clone(),
                            value: None,
                        },
                    };
                    (&kind).into()
                })
                .collect();
            CrdConnectionSource::Url(SingleOrVec::from(kinds))
        }
        ConfigConnectionSource::Params(config) => {
            CrdConnectionSource::Params(Box::new(ConnectionParamsSpec::from(config.as_ref())))
        }
    }
}

#[derive(Debug, Clone)]
pub struct MysqlBranchParams {
    pub name_prefix: String,
    pub labels: BTreeMap<String, String>,
    pub annotations: BTreeMap<String, String>,
    pub spec: MysqlBranchDatabaseSpec,
}

impl MysqlBranchParams {
    pub fn new(id: &str, config: &MysqlBranchConfig, target: &Target) -> Self {
        let name_prefix = format!("{}-mysql-branch-", target.name());
        let connection_source = convert_connection_source(&config.base.connection);
        let spec = MysqlBranchDatabaseSpec {
            id: id.to_owned(),
            database_name: config.base.name.clone(),
            connection_source,
            target: target.clone(),
            ttl_secs: config.base.resolved_ttl_secs(),
            mysql_version: config.base.version.clone(),
            copy: config.copy.clone().into(),
        };
        let labels = BTreeMap::from([(
            labels::MIRRORD_MYSQL_BRANCH_ID_LABEL.to_owned(),
            id.to_owned(),
        )]);
        Self {
            name_prefix,
            labels,
            annotations: BTreeMap::new(),
            spec,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PgBranchParams {
    pub name_prefix: String,
    pub labels: BTreeMap<String, String>,
    pub annotations: BTreeMap<String, String>,
    pub spec: PgBranchDatabaseSpec,
}

impl PgBranchParams {
    pub fn new(id: &str, config: &PgBranchConfig, target: &Target) -> Self {
        let name_prefix = format!("{}-pg-branch-", target.name());
        let connection_source = convert_connection_source(&config.base.connection);

        // Convert IAM auth config if present
        let iam_auth: Option<CrdIamAuthConfig> = config.iam_auth.as_ref().map(Into::into);
        tracing::debug!(?iam_auth, "Converted IAM auth for CRD");
        let spec = PgBranchDatabaseSpec {
            id: id.to_owned(),
            database_name: config.base.name.clone(),
            connection_source,
            target: target.clone(),
            ttl_secs: config.base.resolved_ttl_secs(),
            postgres_version: config.base.version.clone(),
            copy: config.copy.clone().into(),
            iam_auth,
        };
        let labels =
            BTreeMap::from([(labels::MIRRORD_PG_BRANCH_ID_LABEL.to_owned(), id.to_owned())]);
        Self {
            name_prefix,
            labels,
            annotations: BTreeMap::new(),
            spec,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MongodbBranchParams {
    pub name_prefix: String,
    pub labels: BTreeMap<String, String>,
    pub annotations: BTreeMap<String, String>,
    pub spec: MongodbBranchDatabaseSpec,
}

impl MongodbBranchParams {
    pub(crate) fn new(id: &str, config: &MongodbBranchConfig, target: &Target) -> Self {
        let name_prefix = format!("{}-mongodb-branch-", target.name());
        let connection_source = convert_connection_source(&config.base.connection);
        let spec = MongodbBranchDatabaseSpec {
            id: id.to_owned(),
            database_name: config.base.name.clone(),
            connection_source,
            target: target.clone(),
            ttl_secs: config.base.resolved_ttl_secs(),
            mongodb_version: config.base.version.clone(),
            copy: config.copy.clone().into(),
        };
        let labels = BTreeMap::from([(
            labels::MIRRORD_MONGODB_BRANCH_ID_LABEL.to_owned(),
            id.to_owned(),
        )]);
        Self {
            name_prefix,
            labels,
            annotations: BTreeMap::new(),
            spec,
        }
    }
}

/// Returns a label selector fragment that scopes queries to branches owned by the current
/// operator isolation context. When `OPERATOR_ISOLATION_MARKER` is set, matches branches
/// with that marker; otherwise matches branches without any ownership label.
fn ownership_label_selector() -> String {
    match std::env::var(OPERATOR_ISOLATION_MARKER_ENV) {
        Ok(marker) => format!("{}={}", OPERATOR_OWNERSHIP_LABEL, marker),
        Err(_) => format!("!{}", OPERATOR_OWNERSHIP_LABEL),
    }
}

pub mod labels {
    pub(crate) const MIRRORD_MONGODB_BRANCH_ID_LABEL: &str = "mirrord-mongodb-branch-id";
    pub(crate) const MIRRORD_MYSQL_BRANCH_ID_LABEL: &str = "mirrord-mysql-branch-id";
    pub(crate) const MIRRORD_PG_BRANCH_ID_LABEL: &str = "mirrord-pg-branch-id";
    pub const MIRRORD_BRANCH_ID_LABEL: &str = "mirrord-branch-id";
}

pub use crate::crd::TARGET_NAMESPACE_ANNOTATION;
use crate::crd::session::SessionTarget;

/// Possible options to create the [`BranchDatabase`] name
enum GeneratedName {
    /// An explicit name for the resource expected to be the [`ObjectMeta::name`]
    Explicit(String),
    /// A prefix for the [`ObjectMeta::generate_name`] value where k8s will generate the actual
    /// name.
    Generate(String),
}

impl GeneratedName {
    /// Split the name to either a `name` or `generate_name` variables to create [`ObjectMeta`]
    fn into_parts(self) -> (Option<String>, Option<String>) {
        match self {
            GeneratedName::Explicit(name) => (Some(name), None),
            GeneratedName::Generate(name) => (None, Some(name)),
        }
    }
}

/// Create the future [`BranchDatabase`]'s name,
///
/// A user-specified id gets the target-independent `deterministic_name` so that two
/// workloads asking for the same branch id resolve to the same resource and reuse it via
/// the 409 conflict path instead of each creating its own branch. A generated id has no
/// sharing intent, so it uses `generateName` from `name_prefix`, which keeps the target
/// workload name for readability.
///
/// Important: Make sure that the returning value will not exceed the 63 character limit of k8s.
fn generate_branch_name(
    name_prefix: String,
    deterministic_name: String,
    database_id: &BranchDatabaseId,
) -> GeneratedName {
    match database_id {
        BranchDatabaseId::Generated(_) if name_prefix.len() <= 63 => {
            GeneratedName::Generate(name_prefix)
        }
        BranchDatabaseId::Generated(_) => {
            use std::hash::{Hash, Hasher};

            let mut prefix_hasher = std::collections::hash_map::DefaultHasher::new();
            name_prefix.hash(&mut prefix_hasher);

            GeneratedName::Generate(format!("{:x}", prefix_hasher.finish()))
        }
        BranchDatabaseId::Specified(_) => GeneratedName::Explicit(deterministic_name),
    }
}

/// Branch resource name for a user-specified id, independent of the target workload.
///
/// Two workloads sharing the same id must produce the same name so they reuse one branch,
/// so the hash stays stable across mirrord builds and platforms (hence SHA-256). The
/// namespace is mixed in so the same id in two namespaces never collides, and the id is
/// hashed because it can hold characters not allowed in a resource name. The result fits
/// the 63 character limit: `mirrord-<dialect>-branch-<16 hex chars>`.
fn deterministic_branch_name(dialect: &str, target_namespace: &str, id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(target_namespace.as_bytes());
    // Separator so ("ab", "c") and ("a", "bc") can't hash to the same value.
    hasher.update([0]);
    hasher.update(id.as_bytes());
    let digest = hasher.finalize();
    // 8 bytes make collisions effectively impossible and render as exactly 16 hex chars.
    let short_bytes = *digest
        .first_chunk::<8>()
        .expect("a sha256 digest is always 32 bytes long");
    let short = u64::from_be_bytes(short_bytes);
    format!("mirrord-{dialect}-branch-{short:016x}")
}

/// Create unified branch databases and wait for their readiness.
#[tracing::instrument(level = Level::TRACE, skip_all, err, ret)]
pub async fn create_branches<P: Progress>(
    api: &Api<BranchDatabase>,
    params: HashMap<BranchDatabaseId, UnifiedBranchParams>,
    timeout: Duration,
    progress: &P,
) -> Result<HashMap<BranchDatabaseId, BranchDatabase>, OperatorApiError> {
    if params.is_empty() {
        return Ok(HashMap::new());
    }

    let mut subtask = progress.subtask("creating new branch databases");
    let mut created_branches = HashMap::new();
    let mut reused_branches = HashMap::new();

    for (id, params) in params {
        let annotations = if params.annotations.is_empty() {
            None
        } else {
            Some(params.annotations)
        };

        let (name, generate_name) =
            generate_branch_name(params.name_prefix, params.deterministic_name, &id).into_parts();

        let branch = BranchDatabase {
            metadata: ObjectMeta {
                name: name.clone(),
                generate_name,
                labels: Some(params.labels),
                annotations,
                ..Default::default()
            },
            spec: params.spec,
            status: None,
        };

        match api.create(&kube::api::PostParams::default(), &branch).await {
            Ok(branch) => {
                created_branches.insert(id, branch);
            }
            Err(kube::Error::Api(ref err)) if err.code == 409 => {
                if let Some(ref deterministic_name) = name {
                    tracing::info!(
                        name = %deterministic_name,
                        "Branch already exists, reusing"
                    );
                    let existing = api.get(deterministic_name).await.map_err(|e| {
                        OperatorApiError::KubeError {
                            error: e,
                            operation: OperatorOperation::DbBranching,
                        }
                    })?;
                    reused_branches.insert(id, existing);
                }
            }
            Err(e) => {
                return Err(OperatorApiError::KubeError {
                    error: e,
                    operation: OperatorOperation::DbBranching,
                });
            }
        };
    }

    let has_reused = !reused_branches.is_empty();
    created_branches.extend(reused_branches);

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
                .unwrap_or_else(|| "Branch database creation failed".to_owned());
            return Err(OperatorApiError::BranchCreationFailed {
                operation: OperatorOperation::DbBranching,
                message: error_msg,
            });
        }
    }

    if has_reused {
        subtask.success(Some("reusing existing branch databases"));
    } else {
        subtask.success(Some("new branch databases ready"));
    }

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
    params: &HashMap<BranchDatabaseId, UnifiedBranchParams>,
    target_namespace: &str,
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
        // The name is keyed on (target namespace, id), so reuse must be too. Read the
        // target namespace from the annotation, falling back to the branch's own namespace
        // when it isn't set, and skip branches from a different target namespace.
        let branch_target_namespace = db
            .metadata
            .annotations
            .as_ref()
            .and_then(|annotations| annotations.get(TARGET_NAMESPACE_ANNOTATION))
            .map(String::as_str)
            .or(db.metadata.namespace.as_deref());
        if branch_target_namespace != Some(target_namespace) {
            continue;
        }

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
        .filter_map(|(id, db)| db.meta().name.clone().map(|name| (id.clone(), name)))
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
                .unwrap_or_else(|| "Branch database creation failed".to_owned());
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
/// `{{key}}` expansion in the config is already handled by Tera before this point,
/// so `config_id` (if present) is the fully-rendered value.
///
/// - No `id` in config: uses the session key directly (enables automatic reuse)
/// - `id` contains the session key: user likely used `{{key}}` in their template
/// - `id` without the session key: uses the custom ID as-is, warns that the key is unused
pub fn resolve_branch_id<P: Progress>(
    config_id: &Option<String>,
    session_key: &str,
    progress: &P,
) -> BranchDatabaseId {
    match config_id {
        None => BranchDatabaseId::specified(session_key.to_owned()),
        Some(id) if id.contains(session_key) => BranchDatabaseId::specified(id.clone()),
        Some(id) => {
            progress.warning(
                "Custom branch ID does not contain the session key. \
                 Use {{ key }} in your config to include it for automatic branch reuse.",
            );
            BranchDatabaseId::specified(id.clone())
        }
    }
}

pub struct UnifiedDatabaseBranchParams {
    pub branches: HashMap<BranchDatabaseId, UnifiedBranchParams>,
}

impl UnifiedDatabaseBranchParams {
    /// Create unified branch database parameters from user config.
    ///
    /// When no branch `id` is provided, the session key is used as the branch ID so that
    /// sessions sharing the same key automatically reuse the same branch.
    pub fn new<P: Progress>(
        config: &mut DatabaseBranchesConfig,
        target: &Target,
        target_namespace: &str,
        session_key: &str,
        progress: &P,
    ) -> Result<Self, OperatorApiError> {
        let mut target_with_container = target.clone();
        if target_with_container.container().is_none() {
            target_with_container.set_container(String::new());
        }
        let target_display = target_with_container.to_string();
        let session_target = SessionTarget::from_config(target_with_container)
            .ok_or_else(|| OperatorApiError::TargetResolutionFailed(target_display))?;

        let mut branches = HashMap::new();
        for branch_db_config in config.0.iter_mut() {
            let (id_source, connection, migrations_config) = match branch_db_config {
                DatabaseBranchConfig::Clickhouse(c) => (&c.base.id, &mut c.base.connection, None),
                DatabaseBranchConfig::Pg(c) => {
                    (&c.base.id, &mut c.base.connection, c.migrations.as_ref())
                }
                DatabaseBranchConfig::Mysql(c) => {
                    (&c.base.id, &mut c.base.connection, c.migrations.as_ref())
                }
                DatabaseBranchConfig::Dynamodb(c) => (&c.base.id, &mut c.base.connection, None),
                DatabaseBranchConfig::Mongodb(c) => (&c.base.id, &mut c.base.connection, None),
                DatabaseBranchConfig::Mssql(c) => {
                    (&c.base.id, &mut c.base.connection, c.migrations.as_ref())
                }
                DatabaseBranchConfig::Redis(c) => match &mut **c {
                    RedisBranchConfig::Local(_) => continue,
                    RedisBranchConfig::Remote(RemoteRedisBranchConfig { base, .. }) => {
                        (&base.id, &mut base.connection, None)
                    }
                },
                DatabaseBranchConfig::Spanner(c) => (&c.base.id, &mut c.base.connection, None),
            };

            let id = resolve_branch_id(id_source, session_key, progress);
            let mut literal_values = HashMap::new();
            extract_literal_values(connection, &mut literal_values);

            let migrations = read_migrations(migrations_config)?;

            let params = match branch_db_config {
                DatabaseBranchConfig::Clickhouse(c) => UnifiedBranchParams::from_clickhouse(
                    id.as_ref(),
                    c,
                    target,
                    target_namespace,
                    &session_target,
                    literal_values,
                ),
                DatabaseBranchConfig::Pg(c) => UnifiedBranchParams::from_pg(
                    id.as_ref(),
                    c,
                    target,
                    target_namespace,
                    &session_target,
                    literal_values,
                    migrations,
                ),
                DatabaseBranchConfig::Mysql(c) => UnifiedBranchParams::from_mysql(
                    id.as_ref(),
                    c,
                    target,
                    target_namespace,
                    &session_target,
                    literal_values,
                    migrations,
                ),
                DatabaseBranchConfig::Dynamodb(c) => UnifiedBranchParams::from_dynamodb(
                    id.as_ref(),
                    c,
                    target,
                    target_namespace,
                    &session_target,
                    literal_values,
                ),
                DatabaseBranchConfig::Mongodb(c) => UnifiedBranchParams::from_mongodb(
                    id.as_ref(),
                    c,
                    target,
                    target_namespace,
                    &session_target,
                    literal_values,
                    migrations,
                ),
                DatabaseBranchConfig::Mssql(c) => UnifiedBranchParams::from_mssql(
                    id.as_ref(),
                    c,
                    target,
                    target_namespace,
                    &session_target,
                    literal_values,
                    migrations,
                ),
                DatabaseBranchConfig::Redis(c) => match &**c {
                    RedisBranchConfig::Local(_) => unreachable!(),
                    RedisBranchConfig::Remote(c) => UnifiedBranchParams::from_redis(
                        id.as_ref(),
                        c,
                        target,
                        target_namespace,
                        &session_target,
                        literal_values,
                        migrations,
                    ),
                },
                DatabaseBranchConfig::Spanner(c) => UnifiedBranchParams::from_spanner(
                    id.as_ref(),
                    c,
                    target,
                    target_namespace,
                    &session_target,
                    literal_values,
                ),
            };
            branches.insert(id, params);
        }

        if let Ok(marker) = std::env::var(OPERATOR_ISOLATION_MARKER_ENV) {
            for branch_params in branches.values_mut() {
                branch_params
                    .labels
                    .insert(OPERATOR_OWNERSHIP_LABEL.to_owned(), marker.clone());
            }
        }

        Ok(Self { branches })
    }
}

/// Turns the migrations config into the CRD spec.
fn read_migrations(
    config: Option<&SqlBranchMigrationsConfig>,
) -> Result<Option<MigrationsSpec>, OperatorApiError> {
    let Some(config) = config else {
        return Ok(None);
    };

    match config {
        SqlBranchMigrationsConfig::Flyway { path, image } => {
            let archive = build_migration_archive(path).map_err(|error| {
                OperatorApiError::MigrationsRead {
                    path: path.display().to_string(),
                    error: error.to_string(),
                }
            })?;

            const LIMIT: usize = 1024 * 1024;

            if archive.len() > LIMIT {
                return Err(OperatorApiError::MigrationsTooLarge {
                    path: path.display().to_string(),
                    size: archive.len(),
                    limit: LIMIT,
                });
            }

            Ok(Some(MigrationsSpec::Flyway {
                image: image.clone(),
                archive: ByteString(archive),
            }))
        }
    }
}

/// Builds a gzipped tar of a migration directory tree.
///
/// The built tree is deterministic: identical migrations always produce identical bytes.
fn build_migration_archive(path: &std::path::Path) -> std::io::Result<Vec<u8>> {
    let mut files = Vec::new();

    for entry in WalkDir::new(path) {
        let entry = entry.map_err(std::io::Error::from)?;

        if !entry.file_type().is_file() {
            continue;
        }

        let rel = entry
            .path()
            .strip_prefix(path)
            .map_err(std::io::Error::other)?
            .to_string_lossy()
            .into_owned();

        files.push((rel, entry.into_path()));
    }

    files.sort_by(|(a, _), (b, _)| a.cmp(b));

    let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));

    for (rel, abs) in files {
        let contents = std::fs::read(&abs)?;

        let mut header = tar::Header::new_gnu();

        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_cksum();

        builder.append_data(&mut header, &rel, contents.as_slice())?;
    }

    builder.into_inner()?.finish()
}

/// Applies a session's migrations to a branch it is about to use.
///
/// Patches the desired migrations onto the branch and waits for the operator to run them.
///
/// A failure surfaces here so the session doesn't start against a schema it couldn't build.
pub async fn ensure_branch_migrations<P: Progress>(
    api: &Api<BranchDatabase>,
    branch: &BranchDatabase,
    migrations: &MigrationsSpec,
    timeout: Duration,
    progress: &P,
) -> Result<(), OperatorApiError> {
    let name = branch
        .meta()
        .name
        .clone()
        .ok_or(KubeApiError::missing_field(branch, ".metadata.name"))?;

    let mut subtask = progress.subtask("applying branch migrations");

    let patch = Patch::Merge(serde_json::json!({ "spec": { "migrations": migrations } }));

    let patched = api
        .patch(&name, &PatchParams::default(), &patch)
        .await
        .map_err(|error| OperatorApiError::KubeError {
            error,
            operation: OperatorOperation::DbBranching,
        })?;

    let generation = patched.meta().generation.unwrap_or(0);

    let settled = await_condition(api.clone(), &name, move |db: Option<&BranchDatabase>| {
        db.and_then(|db| db.status.as_ref())
            .and_then(|status| status.migrations.as_ref())
            .is_some_and(|run| {
                run.observed_generation >= generation && run.phase != MigrationPhase::Running
            })
    });

    let db = tokio::time::timeout(timeout, settled)
        .await
        .map_err(|_| OperatorApiError::OperationTimeout {
            operation: OperatorOperation::DbBranching,
        })?
        .map_err(|error| OperatorApiError::BranchCreationFailed {
            operation: OperatorOperation::DbBranching,
            message: format!("failed waiting for branch migrations: {error}"),
        })?;

    if let Some(run) = db
        .as_ref()
        .and_then(|db| db.status.as_ref()?.migrations.as_ref())
        && run.phase == MigrationPhase::Failed
    {
        return Err(OperatorApiError::BranchCreationFailed {
            operation: OperatorOperation::DbBranching,
            message: run
                .error
                .clone()
                .unwrap_or_else(|| "branch migrations failed".to_owned()),
        });
    }

    subtask.success(Some("branch migrations applied"));

    Ok(())
}

#[derive(Debug, Clone)]
pub struct UnifiedBranchParams {
    /// Prefix for `generateName`, used only for a branch with a generated id. It keeps the
    /// target workload name so the random resource name stays readable.
    pub name_prefix: String,
    /// Target-independent resource name used for a branch with a user-specified id, so two
    /// workloads sharing the same id map to the same resource and reuse one branch.
    pub deterministic_name: String,
    pub labels: BTreeMap<String, String>,
    pub annotations: BTreeMap<String, String>,
    pub spec: BranchDatabaseSpec,
    pub literal_values: HashMap<String, String>,
}

impl UnifiedBranchParams {
    pub fn from_pg(
        id: &str,
        config: &PgBranchConfig,
        target: &Target,
        target_namespace: &str,
        session_target: &SessionTarget,
        literal_values: HashMap<String, String>,
        migrations: Option<MigrationsSpec>,
    ) -> Self {
        let name_prefix = format!("{}-pg-branch-", target.name());
        let deterministic_name = deterministic_branch_name("pg", target_namespace, id);
        let connection_source = convert_connection_source(&config.base.connection);
        let iam_auth: Option<CrdIamAuthConfig> = config.iam_auth.as_ref().map(Into::into);
        tracing::debug!(?iam_auth, "Converted IAM auth for CRD");

        let spec = BranchDatabaseSpec {
            id: id.to_owned(),
            database_name: config.base.name.clone(),
            connection_source,
            target: session_target.clone(),
            ttl_secs: config.base.resolved_ttl_secs(),
            version: config.base.version.clone(),
            postgres_options: Some(PostgresOptions {
                copy: SqlBranchCopyConfig::from(config.copy.clone()),
                iam_auth,
                connection_settings: config.connection_settings.clone(),
            }),
            mysql_options: None,
            dynamodb_options: None,
            mongodb_options: None,
            mssql_options: None,
            redis_options: None,
            spanner_options: None,
            clickhouse_options: None,
            migrations,
        };
        let labels = BTreeMap::from([(labels::MIRRORD_BRANCH_ID_LABEL.to_owned(), id.to_owned())]);
        Self {
            name_prefix,
            deterministic_name,
            labels,
            annotations: BTreeMap::new(),
            spec,
            literal_values,
        }
    }

    pub fn from_mysql(
        id: &str,
        config: &MysqlBranchConfig,
        target: &Target,
        target_namespace: &str,
        session_target: &SessionTarget,
        literal_values: HashMap<String, String>,
        migrations: Option<MigrationsSpec>,
    ) -> Self {
        let name_prefix = format!("{}-mysql-branch-", target.name());
        let deterministic_name = deterministic_branch_name("mysql", target_namespace, id);
        let connection_source = convert_connection_source(&config.base.connection);
        let iam_auth: Option<CrdIamAuthConfig> = config.iam_auth.as_ref().map(Into::into);
        let spec = BranchDatabaseSpec {
            id: id.to_owned(),
            database_name: config.base.name.clone(),
            connection_source,
            target: session_target.clone(),
            ttl_secs: config.base.resolved_ttl_secs(),
            version: config.base.version.clone(),
            postgres_options: None,
            mysql_options: Some(MysqlOptions {
                copy: SqlBranchCopyConfig::from(config.copy.clone()),
                iam_auth,
            }),
            dynamodb_options: None,
            mongodb_options: None,
            mssql_options: None,
            redis_options: None,
            spanner_options: None,
            clickhouse_options: None,
            migrations,
        };
        let labels = BTreeMap::from([(labels::MIRRORD_BRANCH_ID_LABEL.to_owned(), id.to_owned())]);
        Self {
            name_prefix,
            deterministic_name,
            labels,
            annotations: BTreeMap::new(),
            spec,
            literal_values,
        }
    }

    pub fn from_dynamodb(
        id: &str,
        config: &DynamodbBranchConfig,
        target: &Target,
        target_namespace: &str,
        session_target: &SessionTarget,
        literal_values: HashMap<String, String>,
    ) -> Self {
        let name_prefix = format!("{}-dynamodb-branch-", target.name());
        let deterministic_name = deterministic_branch_name("dynamodb", target_namespace, id);
        let connection_source = convert_connection_source(&config.base.connection);
        let spec = BranchDatabaseSpec {
            id: id.to_owned(),
            database_name: config.base.name.clone(),
            connection_source,
            target: session_target.clone(),
            ttl_secs: config.base.resolved_ttl_secs(),
            version: config.base.version.clone(),
            postgres_options: None,
            mysql_options: None,
            dynamodb_options: Some(DynamodbOptions {
                copy: config.copy.clone().into(),
                iam_auth: config.iam_auth.as_ref().map(Into::into),
            }),
            mongodb_options: None,
            mssql_options: None,
            redis_options: None,
            spanner_options: None,
            clickhouse_options: None,
            migrations: None,
        };
        let labels = BTreeMap::from([(labels::MIRRORD_BRANCH_ID_LABEL.to_owned(), id.to_owned())]);
        Self {
            name_prefix,
            deterministic_name,
            labels,
            annotations: BTreeMap::new(),
            spec,
            literal_values,
        }
    }

    pub fn from_mongodb(
        id: &str,
        config: &MongodbBranchConfig,
        target: &Target,
        target_namespace: &str,
        session_target: &SessionTarget,
        literal_values: HashMap<String, String>,
        migrations: Option<MigrationsSpec>,
    ) -> Self {
        let name_prefix = format!("{}-mongodb-branch-", target.name());
        let deterministic_name = deterministic_branch_name("mongodb", target_namespace, id);
        let connection_source = convert_connection_source(&config.base.connection);
        let spec = BranchDatabaseSpec {
            id: id.to_owned(),
            database_name: config.base.name.clone(),
            connection_source,
            target: session_target.clone(),
            ttl_secs: config.base.resolved_ttl_secs(),
            version: config.base.version.clone(),
            postgres_options: None,
            mysql_options: None,
            dynamodb_options: None,
            mongodb_options: Some(MongodbOptions {
                copy: config.copy.clone().into(),
            }),
            mssql_options: None,
            redis_options: None,
            spanner_options: None,
            clickhouse_options: None,
            migrations,
        };
        let labels = BTreeMap::from([(labels::MIRRORD_BRANCH_ID_LABEL.to_owned(), id.to_owned())]);
        Self {
            name_prefix,
            deterministic_name,
            labels,
            annotations: BTreeMap::new(),
            spec,
            literal_values,
        }
    }

    pub fn from_mssql(
        id: &str,
        config: &mirrord_config::feature::database_branches::MssqlBranchConfig,
        target: &Target,
        target_namespace: &str,
        session_target: &SessionTarget,
        literal_values: HashMap<String, String>,
        migrations: Option<MigrationsSpec>,
    ) -> Self {
        let name_prefix = format!("{}-mssql-branch-", target.name());
        let deterministic_name = deterministic_branch_name("mssql", target_namespace, id);
        let connection_source = convert_connection_source(&config.base.connection);
        let spec = BranchDatabaseSpec {
            id: id.to_owned(),
            database_name: config.base.name.clone(),
            connection_source,
            target: session_target.clone(),
            ttl_secs: config.base.resolved_ttl_secs(),
            version: config.base.version.clone(),
            postgres_options: None,
            mysql_options: None,
            dynamodb_options: None,
            mongodb_options: None,
            mssql_options: Some(MssqlOptions {
                copy: config.copy.clone().into(),
            }),
            redis_options: None,
            spanner_options: None,
            clickhouse_options: None,
            migrations,
        };
        let labels = BTreeMap::from([(labels::MIRRORD_BRANCH_ID_LABEL.to_owned(), id.to_owned())]);
        Self {
            name_prefix,
            deterministic_name,
            labels,
            annotations: BTreeMap::new(),
            spec,
            literal_values,
        }
    }

    pub fn from_redis(
        id: &str,
        config: &RemoteRedisBranchConfig,
        target: &Target,
        target_namespace: &str,
        session_target: &SessionTarget,
        literal_values: HashMap<String, String>,
        migrations: Option<MigrationsSpec>,
    ) -> Self {
        let name_prefix = format!("{}-redis-branch-", target.name());
        let deterministic_name = deterministic_branch_name("redis", target_namespace, id);
        let connection_source = convert_connection_source(&config.base.connection);
        let spec = BranchDatabaseSpec {
            id: id.to_owned(),
            database_name: config.base.name.clone(),
            connection_source,
            target: session_target.clone(),
            ttl_secs: config.base.resolved_ttl_secs(),
            version: config.base.version.clone(),
            postgres_options: None,
            mysql_options: None,
            dynamodb_options: None,
            mongodb_options: None,
            mssql_options: None,
            redis_options: Some(RedisOptions {
                copy: config.copy.clone().into(),
            }),
            clickhouse_options: None,
            spanner_options: None,
            migrations,
        };
        let labels = BTreeMap::from([(labels::MIRRORD_BRANCH_ID_LABEL.to_owned(), id.to_owned())]);
        Self {
            name_prefix,
            deterministic_name,
            labels,
            annotations: BTreeMap::new(),
            spec,
            literal_values,
        }
    }

    pub fn from_clickhouse(
        id: &str,
        config: &ClickhouseBranchConfig,
        target: &Target,
        target_namespace: &str,
        session_target: &SessionTarget,
        literal_values: HashMap<String, String>,
    ) -> Self {
        let name_prefix = format!("{}-clickhouse-branch-", target.name());
        let deterministic_name = deterministic_branch_name("clickhouse", target_namespace, id);
        let connection_source = convert_connection_source(&config.base.connection);
        let spec = BranchDatabaseSpec {
            id: id.to_owned(),
            database_name: config.base.name.clone(),
            connection_source,
            target: session_target.clone(),
            ttl_secs: config.base.resolved_ttl_secs(),
            version: config.base.version.clone(),
            postgres_options: None,
            mysql_options: None,
            dynamodb_options: None,
            mongodb_options: None,
            mssql_options: None,
            redis_options: None,
            clickhouse_options: Some(ClickhouseOptions {
                copy: config.copy.clone().into(),
            }),
            migrations: None,
            spanner_options: None,
        };
        let labels = BTreeMap::from([(labels::MIRRORD_BRANCH_ID_LABEL.to_owned(), id.to_owned())]);
        Self {
            name_prefix,
            deterministic_name,
            labels,
            annotations: BTreeMap::new(),
            spec,
            literal_values,
        }
    }

    pub fn from_spanner(
        id: &str,
        config: &SpannerBranchConfig,
        target: &Target,
        target_namespace: &str,
        session_target: &SessionTarget,
        literal_values: HashMap<String, String>,
    ) -> Self {
        let name_prefix = format!("{}-spanner-branch-", target.name());
        let deterministic_name = deterministic_branch_name("spanner", target_namespace, id);

        // Spanner keeps the app's project/instance/database untouched (only SPANNER_EMULATOR_HOST
        // is injected), so its source identifiers live flat in `connection.params` under the
        // `project` / `instance` / `database_id` keys rather than the fixed slots, which would
        // trigger a generic connection override. The shared converter carries those flattened keys
        // through to the CRD's `extra`; the operator validates them against SpannerParam and
        // resolves each from the target pod so the init sidecar can recreate and copy them.
        let connection_source = convert_connection_source(&config.base.connection);

        let spec = BranchDatabaseSpec {
            id: id.to_owned(),
            database_name: config.base.name.clone(),
            connection_source,
            target: session_target.clone(),
            ttl_secs: config.base.resolved_ttl_secs(),
            version: config.base.version.clone(),
            postgres_options: None,
            mysql_options: None,
            dynamodb_options: None,
            mongodb_options: None,
            mssql_options: None,
            redis_options: None,
            clickhouse_options: None,
            spanner_options: Some(SpannerOptions {
                copy: config.copy.clone().into(),
                emulator_host_var: Some(config.emulator_host.clone()),
            }),
            migrations: None,
        };
        let labels = BTreeMap::from([(labels::MIRRORD_BRANCH_ID_LABEL.to_owned(), id.to_owned())]);
        Self {
            name_prefix,
            deterministic_name,
            labels,
            annotations: BTreeMap::new(),
            spec,
            literal_values,
        }
    }
}

#[cfg(test)]
mod test {
    use mirrord_progress::NullProgress;

    use super::{BranchDatabaseId, build_migration_archive, resolve_branch_id};

    #[test]
    fn migration_archive_is_deterministic() {
        let dir = tempfile::tempdir().unwrap();

        std::fs::write(dir.path().join("V1__a.sql"), b"create table a ();").unwrap();

        std::fs::create_dir(dir.path().join("nested")).unwrap();

        std::fs::write(
            dir.path().join("nested/V2__b.sql"),
            b"insert into a values ();",
        )
        .unwrap();

        let first = build_migration_archive(dir.path()).unwrap();
        let second = build_migration_archive(dir.path()).unwrap();

        assert_eq!(
            first, second,
            "identical files must build identical archives"
        );

        std::fs::write(dir.path().join("V1__a.sql"), b"create table a (id int);").unwrap();

        let changed = build_migration_archive(dir.path()).unwrap();

        assert_ne!(first, changed, "a content change must change the archive");
    }

    #[test]
    fn no_id_uses_session_key() {
        let id = resolve_branch_id(&None, "my-session-key", &NullProgress);
        assert_eq!(id, BranchDatabaseId::Specified("my-session-key".to_owned()));
    }

    #[test]
    fn custom_id_containing_session_key_is_recognized() {
        // Simulates Tera having already expanded `{{key}}` in "branch-{{key}}-db"
        let config_id = Some("branch-abc123-db".to_owned());
        let id = resolve_branch_id(&config_id, "abc123", &NullProgress);
        assert_eq!(
            id,
            BranchDatabaseId::Specified("branch-abc123-db".to_owned())
        );
    }

    #[test]
    fn custom_id_equal_to_session_key() {
        // Simulates Tera having expanded a config id that was just `{{key}}`
        let config_id = Some("full-key".to_owned());
        let id = resolve_branch_id(&config_id, "full-key", &NullProgress);
        assert_eq!(id, BranchDatabaseId::Specified("full-key".to_owned()));
    }

    #[test]
    fn custom_id_without_session_key_used_as_is() {
        let config_id = Some("fixed-branch-id".to_owned());
        let id = resolve_branch_id(&config_id, "ignored-key", &NullProgress);
        assert_eq!(
            id,
            BranchDatabaseId::Specified("fixed-branch-id".to_owned())
        );
    }

    #[test]
    fn custom_id_with_key_as_substring() {
        // Key appears as a substring, e.g. user wrote "prefix-{{key}}-suffix"
        // and Tera expanded it to "prefix-mykey-suffix"
        let config_id = Some("prefix-mykey-suffix".to_owned());
        let id = resolve_branch_id(&config_id, "mykey", &NullProgress);
        assert_eq!(
            id,
            BranchDatabaseId::Specified("prefix-mykey-suffix".to_owned())
        );
    }

    #[test]
    fn session_key_with_special_characters() {
        let id = resolve_branch_id(&None, "key/with:special@chars", &NullProgress);
        assert_eq!(
            id,
            BranchDatabaseId::Specified("key/with:special@chars".to_owned())
        );
    }

    #[test]
    fn all_branches_produce_specified_variant() {
        let cases: Vec<(Option<String>, &str)> = vec![
            (None, "session-key"),
            (Some("id-with-session-key-inside".to_owned()), "session-key"),
            (Some("static-id".to_owned()), "session-key"),
        ];
        for (config_id, key) in cases {
            let id = resolve_branch_id(&config_id, key, &NullProgress);
            assert!(
                matches!(id, BranchDatabaseId::Specified(_)),
                "expected Specified variant for config_id={config_id:?}, key={key}"
            );
        }
    }
}
