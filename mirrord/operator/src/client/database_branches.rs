use std::{
    collections::{BTreeMap, HashMap},
    iter,
    time::Duration,
};

use flate2::{Compression, write::GzEncoder};
use k8s_openapi::ByteString;
use kube::{
    Api, Resource, ResourceExt,
    api::{ListParams, ObjectMeta, Patch, PatchParams},
    runtime::wait::await_condition,
};
use mirrord_config::{
    feature::database_branches::{
        ClickhouseBranchConfig, CockroachdbBranchConfig, ConnectionParamsConfig,
        ConnectionSource as ConfigConnectionSource, ConnectionSourceType, DatabaseBranchConfig,
        DatabaseBranchesConfig, DynamodbBranchConfig, GenericBranchConfig, GenericReadinessConfig,
        MariadbBranchConfig, MongodbBranchConfig, MysqlBranchConfig, ParamSource,
        PgAdditionalDatabaseConfig, PgBranchConfig, RedisBranchConfig, S3BranchConfig, SingleOrVec,
        SpannerBranchConfig, SqlBranchMigrationsConfig, TargetEnvironmentVariableSource,
        TurbopufferBranchConfig, redis::RemoteRedisBranchConfig,
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
            BranchDatabase, BranchDatabaseSpec, ClickhouseOptions, CockroachdbOptions,
            DynamodbOptions, GenericCopySpec, GenericExecProbeSpec, GenericHttpGetProbeSpec,
            GenericOptions, GenericReadinessSpec, MariadbOptions, MigrationsSpec, MongodbOptions,
            MssqlOptions, MysqlOptions, PgAdditionalDatabase, PostgresOptions, RedisOptions,
            S3Options, SpannerOptions, SqlBranchCopyConfig, TurbopufferOptions,
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

    // Wait for either Ready or Failed phase
    let ready_or_failed = branch_names
        .iter()
        .map(|name| {
            await_condition(api.clone(), name, |db: Option<&MongodbBranchDatabase>| {
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
            operation: OperatorOperation::MongodbBranching,
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
                operation: OperatorOperation::MongodbBranching,
                message: error_msg,
            });
        }
    }

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
        for branch_db_config in config.iter() {
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
                | DatabaseBranchConfig::Spanner(_)
                | DatabaseBranchConfig::Mariadb(_)
                | DatabaseBranchConfig::Clickhouse(_)
                | DatabaseBranchConfig::Cockroachdb(_)
                | DatabaseBranchConfig::Generic(_)
                | DatabaseBranchConfig::S3(_)
                | DatabaseBranchConfig::Turbopuffer(_) => {}
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
        ConfigConnectionSource::Params(config) => extract_literal_param_values(config, values_out),
    }
}

/// [`extract_literal_values`] for a branch that declares its source as params only, with no
/// [`ConfigConnectionSource`] wrapping them (S3).
#[cfg(feature = "client")]
pub fn extract_literal_param_values(
    config: &mut ConnectionParamsConfig,
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

    for param in [
        &mut config.params.url,
        &mut config.params.host,
        &mut config.params.port,
        &mut config.params.user,
        &mut config.params.password,
        &mut config.params.database,
    ]
    .into_iter()
    .flatten()
    .flat_map(|om| om.0.iter_mut())
    // Custom `extra` params carry credentials for generic branches, locators for Spanner and
    // the bucket for S3; a literal value in one of them must land in the credential Secret
    // like the fixed slots, not in the CRD in plaintext.
    .chain(
        config
            .params
            .extra
            .values_mut()
            .flat_map(|om| om.0.iter_mut()),
    ) {
        extract_from_param(param, values_out);
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
        if let ConnectionSourceKind::Env { variable, .. }
        | ConnectionSourceKind::EnvFrom { variable, .. } = kind
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
                &mut params.url,
                &mut params.host,
                &mut params.port,
                &mut params.user,
                &mut params.password,
                &mut params.database,
            ]
            .into_iter()
            .flatten()
            .flatten()
            .chain(params.extra.values_mut().flatten())
            {
                replace_kind(kind, secret_name, literal_values);
            }
        }
    }
}

/// [`replace_values_with_secret_refs`] over every connection a branch spec carries: its own and
/// those of a PostgreSQL branch's additional databases. The CLI extracted literal values from
/// all of them into the same Secret.
#[cfg(feature = "client")]
pub fn replace_spec_values_with_secret_refs(
    spec: &mut BranchDatabaseSpec,
    secret_name: &str,
    literal_values: &std::collections::HashMap<String, String>,
) {
    let additional_sources = spec
        .postgres_options
        .iter_mut()
        .flat_map(|options| options.additional_databases.iter_mut())
        .filter_map(|database| database.connection_source.as_mut());
    for source in iter::once(&mut spec.connection_source).chain(additional_sources) {
        replace_values_with_secret_refs(source, secret_name, literal_values);
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

fn convert_additional_database(config: &PgAdditionalDatabaseConfig) -> PgAdditionalDatabase {
    PgAdditionalDatabase {
        name: config.name.trim().to_owned(),
        connection_source: config.connection.as_ref().map(convert_connection_source),
        copy: SqlBranchCopyConfig::from(config.copy.clone()),
    }
}

/// The key a PostgreSQL branch is found and reused by, hashed into its resource name.
///
/// A branch is only reused by a session asking for the same additional databases, connected
/// the same way. A branch without additional databases keeps the plain id and so the name it
/// always had.
fn pg_reuse_key(id: &str, additional: &[PgAdditionalDatabase]) -> String {
    if additional.is_empty() {
        return id.to_owned();
    }

    let mut databases = additional
        .iter()
        .map(|database| (database.name.as_str(), database.connection_source.as_ref()))
        .collect::<Vec<_>>();
    databases.sort_unstable_by_key(|(name, _)| *name);

    serde_json::to_string(&(id, databases)).expect("a connection config always serializes to JSON")
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
        let connection_source = convert_connection_source(&config.database.connection);
        let spec = MysqlBranchDatabaseSpec {
            id: id.to_owned(),
            database_name: config.database.name.clone(),
            connection_source,
            target: target.clone(),
            ttl_secs: config.base.resolved_ttl_secs(),
            mysql_version: config.pod.version.clone(),
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
        let connection_source = convert_connection_source(&config.database.connection);

        // Convert IAM auth config if present
        let iam_auth: Option<CrdIamAuthConfig> = config.iam_auth.as_ref().map(Into::into);
        tracing::debug!(?iam_auth, "Converted IAM auth for CRD");
        let spec = PgBranchDatabaseSpec {
            id: id.to_owned(),
            database_name: config.database.name.clone(),
            connection_source,
            target: target.clone(),
            ttl_secs: config.base.resolved_ttl_secs(),
            postgres_version: config.pod.version.clone(),
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
        let connection_source = convert_connection_source(&config.database.connection);
        let spec = MongodbBranchDatabaseSpec {
            id: id.to_owned(),
            database_name: config.database.name.clone(),
            connection_source,
            target: target.clone(),
            ttl_secs: config.base.resolved_ttl_secs(),
            mongodb_version: config.pod.version.clone(),
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
use crate::crd::session::{KubeResourceTarget, SessionTarget};

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

/// Outcome of [`create_branches`], kept apart so the caller can say which is which. Each branch
/// is as it was once ready, status included.
#[derive(Debug, Default)]
pub struct CreatedBranches {
    /// Branches this session minted.
    pub created: HashMap<String, BranchDatabase>,
    /// Branches another session minted between this session's lookup and its create, found
    /// through the create conflict and picked up instead.
    pub reused: HashMap<String, BranchDatabase>,
}

/// Create unified branch databases and wait for their readiness.
#[tracing::instrument(level = Level::TRACE, skip_all, err, ret)]
pub async fn create_branches<P: Progress>(
    api: &Api<BranchDatabase>,
    params: HashMap<String, UnifiedBranchParams>,
    timeout: Duration,
    progress: &P,
) -> Result<CreatedBranches, OperatorApiError> {
    if params.is_empty() {
        return Ok(CreatedBranches::default());
    }

    let mut subtask = progress.subtask("creating new branch databases");
    let mut created_branches = HashMap::new();
    let mut reused_branches = HashMap::new();

    for (name, params) in params {
        let annotations = if params.annotations.is_empty() {
            None
        } else {
            Some(params.annotations)
        };

        let branch = BranchDatabase {
            metadata: ObjectMeta {
                name: Some(name.clone()),
                labels: Some(params.labels),
                annotations,
                ..Default::default()
            },
            spec: params.spec,
            status: None,
        };

        match api.create(&kube::api::PostParams::default(), &branch).await {
            Ok(branch) => {
                created_branches.insert(name, branch);
            }
            Err(kube::Error::Api(ref err)) if err.code == 409 => {
                // The lookup before this create came back empty, so another session minted the
                // branch in between. Say so, or the user sees "1 to create" followed by a reuse
                // with no explanation.
                subtask.info(&format!(
                    "branch database {name} was created by another session meanwhile, reusing it"
                ));
                let existing = api
                    .get(&name)
                    .await
                    .map_err(|e| OperatorApiError::KubeError {
                        error: e,
                        operation: OperatorOperation::DbBranching,
                    })?;
                reused_branches.insert(name, existing);
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
    let mut outcome = CreatedBranches {
        created: created_branches,
        reused: reused_branches,
    };

    let branch_names = outcome
        .created
        .values()
        .chain(outcome.reused.values())
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

        let name = db.name_any();

        if let Some(branch) = outcome
            .created
            .get_mut(&name)
            .or(outcome.reused.get_mut(&name))
        {
            *branch = db;
        }
    }

    if has_reused {
        subtask.success(Some("reusing existing branch databases"));
    } else {
        subtask.success(Some("new branch databases ready"));
    }

    Ok(outcome)
}

/// Condition type the operator sets to `False` on a branch whose source differs from it in a way
/// a copy cannot paper over, such as the server versions. The branch still comes up.
const SOURCE_COMPATIBLE_CONDITION: &str = "SourceCompatible";

/// Shows the operator's verdict on `db` to whoever is about to use it.
///
/// The condition is advisory and the operator states it once, when the branch comes up, so a
/// session that reuses an existing branch has to read it off the resource or never hear about it.
/// Call this for every branch a session takes on, whichever path produced it, not only the ones
/// this session created.
pub fn relay_source_compatibility_warnings<P: Progress>(db: &BranchDatabase, progress: &P) {
    let conditions = db.status.iter().flat_map(|status| &status.conditions);

    for condition in conditions {
        if condition.type_ == SOURCE_COMPATIBLE_CONDITION && condition.status == "False" {
            progress.warning(&format!("{}: {}", db.spec.id, condition.message));
        }
    }
}

/// Branches found under the requested resource names, sorted by what the caller does with them.
#[derive(Default)]
pub struct ExistingBranches {
    /// Branches already in Ready phase, can be used immediately.
    pub ready: HashMap<String, BranchDatabase>,
    /// Branches still being created (not Ready, not Failed). The caller should wait
    /// for these instead of creating duplicates.
    pub pending: HashMap<String, BranchDatabase>,
    /// Branches that failed to come up. They occupy the resource name a fresh branch would
    /// take, so the caller must report them instead of trying to create over them.
    pub failed: HashMap<String, BranchDatabase>,
}

/// Sort branches found under the requested resource names by phase.
///
/// Every branch here is one the caller asked for, so the phase is the only thing left to
/// decide: Ready is reusable now, Failed is dead, and anything else (Init,
/// Pending, no status yet, or a phase this build does not know) is still coming up.
fn classify_existing_branches(
    found: impl IntoIterator<Item = (String, BranchDatabase)>,
) -> ExistingBranches {
    let mut existing = ExistingBranches::default();
    for (name, db) in found {
        let bucket = match db.status.as_ref().map(|status| &status.phase) {
            Some(BranchDatabasePhase::Ready) => &mut existing.ready,
            Some(BranchDatabasePhase::Failed) => &mut existing.failed,
            _ => &mut existing.pending,
        };
        bucket.insert(name, db);
    }
    existing
}

/// The connection mapping this session sends along for the branches it reuses, keyed by branch
/// resource name.
///
/// The operator rewrites a session's env vars using a branch's `spec.connectionSource`, which
/// names the vars of the workload that CREATED the branch. A workload reusing the branch under
/// the same `id` may read its connection from other vars (`AUDIT_DB_HOST` where the creator has
/// `DB_HOST`), so for those the session's own mapping (`requested`, from this config) has to
/// reach the operator. A reused branch whose spec already equals what this session declares is
/// left out, so the common same-config reuse sends nothing extra and behaves as before.
///
/// Both inputs are keyed by the branch resource name, the same key the lookup and create
/// paths use.
pub fn reused_branch_connection_sources<'a>(
    requested: &HashMap<String, CrdConnectionSource>,
    reused: impl IntoIterator<Item = (&'a String, &'a BranchDatabase)>,
) -> BTreeMap<String, CrdConnectionSource> {
    reused
        .into_iter()
        .filter_map(|(name, branch)| {
            let source = requested.get(name)?;
            if branch.spec.connection_source == *source {
                return None;
            }
            Some((name.clone(), source.clone()))
        })
        .collect()
}

/// Look up the branch databases that already exist under the resource names in `params`.
///
/// The lookup goes by the same deterministic resource name that [`create_branches`] uses,
/// so its answer is exactly what a create would run into: a branch it finds is the one a
/// create would collide with, and a branch it misses is one a create can mint. Listing by
/// the id label and filtering on the target-namespace annotation used a different key,
/// and when the two disagreed the user saw "0 ready, 0 pending" followed by a silent
/// reuse on the create conflict.
pub async fn list_existing_branches<P: Progress>(
    api: &Api<BranchDatabase>,
    params: &HashMap<String, UnifiedBranchParams>,
    progress: &P,
) -> Result<ExistingBranches, OperatorApiError> {
    if params.is_empty() {
        return Ok(ExistingBranches::default());
    }

    let mut subtask = progress.subtask("looking up existing branch databases");

    let lookups = params
        .keys()
        .map(|name| async move { api.get_opt(name).await.map(|found| (name.clone(), found)) });
    let found = futures::future::try_join_all(lookups)
        .await
        .map_err(|e| OperatorApiError::KubeError {
            error: e,
            operation: OperatorOperation::DbBranching,
        })?
        .into_iter()
        .filter_map(|(name, found)| found.map(|db| (name, db)));
    let existing = classify_existing_branches(found);

    // A failed branch is not creatable either: it holds the name, and the caller reports it.
    let to_create =
        params.len() - existing.ready.len() - existing.pending.len() - existing.failed.len();
    let mut summary = format!(
        "{} ready to reuse, {} still initializing, {} to create",
        existing.ready.len(),
        existing.pending.len(),
        to_create,
    );
    if !existing.failed.is_empty() {
        summary.push_str(&format!(", {} failed", existing.failed.len()));
    }
    subtask.success(Some(&summary));
    Ok(existing)
}

/// Wait for pending branch databases to become Ready or Failed.
///
/// Returns the branches that reached Ready. Returns an error if any branch failed.
pub async fn wait_for_pending_branches<P: Progress>(
    api: &Api<BranchDatabase>,
    pending: &HashMap<String, BranchDatabase>,
    timeout: Duration,
    progress: &P,
) -> Result<HashMap<String, BranchDatabase>, OperatorApiError> {
    if pending.is_empty() {
        return Ok(HashMap::new());
    }

    let mut subtask = progress.subtask("waiting for in-progress branch databases");

    let wait_futures = pending
        .keys()
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
    let results = tokio::time::timeout(timeout, futures::future::join_all(wait_futures))
        .await
        .map_err(|_| OperatorApiError::OperationTimeout {
            operation: OperatorOperation::DbBranching,
        })?;

    let mut ready_branches = HashMap::new();
    for (result, name) in results.into_iter().zip(pending.keys()) {
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

        ready_branches.insert(name.clone(), db);
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
    /// Keyed by each branch's resource name, so entries of different types, or with different
    /// additional databases, stay apart even under one id.
    pub branches: HashMap<String, UnifiedBranchParams>,
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
        let session_target = match SessionTarget::from_config(target_with_container)
            .ok_or_else(|| OperatorApiError::TargetResolutionFailed(target_display.clone()))?
        {
            SessionTarget::KubeResource(target) => target,
            SessionTarget::PodSet(_) => {
                return Err(OperatorApiError::UnsupportedTargetConfig(format!(
                    "database branching does not support pod-set target `{target_display}`"
                )));
            }
        };

        let mut branches = HashMap::new();
        // Where each branch was configured, and whether that entry set an `id`.
        let mut entries = HashMap::new();
        // An unresolved `"*"` / ids request never gets here: `prepare_branch_dbs` resolves it
        // into inline entries first.
        let inline = config
            .inline_mut()
            .map(Vec::as_mut_slice)
            .unwrap_or_default();
        for (position, branch_db_config) in inline.iter_mut().enumerate() {
            // Local Redis branches are run by the CLI itself and never reach the operator,
            // and they are the only branches without the shared base.
            let Some(base) = branch_db_config.base() else {
                continue;
            };
            let sets_id = base.id.is_some();
            let id = resolve_branch_id(&base.id, session_key, progress);

            let migrations = read_migrations(branch_db_config.migrations())?;

            let mut literal_values = HashMap::new();
            match &mut *branch_db_config {
                // A pod-less branch has no source database to reach its params through.
                DatabaseBranchConfig::S3(config) => {
                    extract_literal_param_values(&mut config.source, &mut literal_values)
                }
                DatabaseBranchConfig::Turbopuffer(config) => {
                    extract_literal_param_values(&mut config.source, &mut literal_values)
                }
                other => {
                    if let Some(database) = other.database_mut() {
                        extract_literal_values(&mut database.connection, &mut literal_values);
                    }
                }
            }
            // The additional databases' app connections are rewritten like the branch's own,
            // so their literal values go to the same Secret instead of into the CRD.
            if let DatabaseBranchConfig::Pg(config) = &mut *branch_db_config {
                for connection in config
                    .additional_databases
                    .iter_mut()
                    .filter_map(|database| database.connection.as_mut())
                {
                    extract_literal_values(connection, &mut literal_values);
                }
            }

            let params = match branch_db_config {
                DatabaseBranchConfig::Clickhouse(c) => UnifiedBranchParams::from_clickhouse(
                    id.as_ref(),
                    c,
                    target_namespace,
                    &session_target,
                    literal_values,
                ),
                DatabaseBranchConfig::Cockroachdb(c) => UnifiedBranchParams::from_cockroachdb(
                    id.as_ref(),
                    c,
                    target_namespace,
                    &session_target,
                    literal_values,
                    migrations,
                ),
                DatabaseBranchConfig::Pg(c) => UnifiedBranchParams::from_pg(
                    id.as_ref(),
                    c,
                    target_namespace,
                    &session_target,
                    literal_values,
                    migrations,
                ),
                DatabaseBranchConfig::Mysql(c) => UnifiedBranchParams::from_mysql(
                    id.as_ref(),
                    c,
                    target_namespace,
                    &session_target,
                    literal_values,
                    migrations,
                ),
                DatabaseBranchConfig::Mariadb(c) => UnifiedBranchParams::from_mariadb(
                    id.as_ref(),
                    c,
                    target_namespace,
                    &session_target,
                    literal_values,
                    migrations,
                ),
                DatabaseBranchConfig::Dynamodb(c) => UnifiedBranchParams::from_dynamodb(
                    id.as_ref(),
                    c,
                    target_namespace,
                    &session_target,
                    literal_values,
                ),
                DatabaseBranchConfig::Mongodb(c) => UnifiedBranchParams::from_mongodb(
                    id.as_ref(),
                    c,
                    target_namespace,
                    &session_target,
                    literal_values,
                    migrations,
                ),
                DatabaseBranchConfig::Mssql(c) => UnifiedBranchParams::from_mssql(
                    id.as_ref(),
                    c,
                    target_namespace,
                    &session_target,
                    literal_values,
                    migrations,
                ),
                DatabaseBranchConfig::Redis(c) => match &**c {
                    RedisBranchConfig::Local(_) => {
                        unreachable!("local Redis branches are skipped above")
                    }
                    RedisBranchConfig::Remote(c) => UnifiedBranchParams::from_redis(
                        id.as_ref(),
                        c,
                        target_namespace,
                        &session_target,
                        literal_values,
                        migrations,
                    ),
                },
                DatabaseBranchConfig::Spanner(c) => UnifiedBranchParams::from_spanner(
                    id.as_ref(),
                    c,
                    target_namespace,
                    &session_target,
                    literal_values,
                ),
                DatabaseBranchConfig::Generic(c) => UnifiedBranchParams::from_generic(
                    id.as_ref(),
                    c,
                    target_namespace,
                    &session_target,
                    literal_values,
                ),
                DatabaseBranchConfig::S3(c) => UnifiedBranchParams::from_s3(
                    id.as_ref(),
                    c,
                    target_namespace,
                    &session_target,
                    literal_values,
                ),
                DatabaseBranchConfig::Turbopuffer(c) => UnifiedBranchParams::from_turbopuffer(
                    id.as_ref(),
                    c,
                    target_namespace,
                    &session_target,
                    literal_values,
                ),
            };
            let name = params.deterministic_name.clone();
            if let Some((earlier, earlier_sets_id)) =
                entries.insert(name.clone(), (position, sets_id))
            {
                let reason = if earlier_sets_id || sets_id {
                    format!("both use the id `{id}`")
                } else {
                    "neither sets an `id`".to_owned()
                };
                return Err(OperatorApiError::BranchCreationFailed {
                    operation: OperatorOperation::DbBranching,
                    message: format!(
                        "`feature.db_branches[{earlier}]` and `feature.db_branches[{position}]` \
                         are the same branch because {reason}; give each its own `id`"
                    ),
                });
            }
            branches.insert(name, params);
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
        SqlBranchMigrationsConfig::Flyway {
            path: Some(path),
            image,
            locations: _,
        } => Ok(Some(MigrationsSpec::Flyway {
            image: image.clone(),
            archive: Some(read_migration_archive(path)?),
            locations: Vec::new(),
        })),
        // Image-native Flyway: the migration files live inside `image`, so nothing is uploaded;
        // the operator runs Flyway against the in-image `locations`.
        SqlBranchMigrationsConfig::Flyway {
            path: None,
            image,
            locations,
        } => Ok(Some(MigrationsSpec::Flyway {
            image: image.clone(),
            archive: None,
            locations: locations.clone(),
        })),
        SqlBranchMigrationsConfig::Liquibase {
            path: Some(path),
            image,
            changelog_file,
            search_path: _,
        } => Ok(Some(MigrationsSpec::Liquibase {
            image: image.clone(),
            archive: Some(read_migration_archive(path)?),
            changelog_file: changelog_file.clone(),
            search_path: Vec::new(),
        })),
        // Image-native: the changelogs live inside `image`, so nothing is uploaded.
        SqlBranchMigrationsConfig::Liquibase {
            path: None,
            image,
            changelog_file,
            search_path,
        } => Ok(Some(MigrationsSpec::Liquibase {
            image: image.clone(),
            archive: None,
            changelog_file: changelog_file.clone(),
            search_path: search_path.clone(),
        })),
        SqlBranchMigrationsConfig::Container {
            image,
            command,
            args,
            env,
        } => Ok(Some(MigrationsSpec::Container {
            image: image.clone(),
            command: command.clone(),
            args: args.clone(),
            env: env.clone(),
        })),
    }
}

/// Builds the upload archive for a local migrations directory, within the size limit the
/// operator's ConfigMap can carry.
fn read_migration_archive(path: &std::path::Path) -> Result<ByteString, OperatorApiError> {
    let archive =
        build_migration_archive(path).map_err(|error| OperatorApiError::MigrationsRead {
            path: path.display().to_string(),
            error: error.to_string(),
        })?;

    const LIMIT: usize = 1024 * 1024;

    if archive.len() > LIMIT {
        return Err(OperatorApiError::MigrationsTooLarge {
            path: path.display().to_string(),
            size: archive.len(),
            limit: LIMIT,
        });
    }

    Ok(ByteString(archive))
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
                run.observed_generation >= generation
                    && matches!(
                        run.phase,
                        MigrationPhase::Succeeded | MigrationPhase::Failed
                    )
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
        target_namespace: &str,
        session_target: &KubeResourceTarget,
        literal_values: HashMap<String, String>,
        migrations: Option<MigrationsSpec>,
    ) -> Self {
        let additional_databases = config
            .additional_databases
            .iter()
            .map(convert_additional_database)
            .collect::<Vec<_>>();
        let reuse_key = pg_reuse_key(id, &additional_databases);
        let deterministic_name = deterministic_branch_name("pg", target_namespace, &reuse_key);
        let connection_source = convert_connection_source(&config.database.connection);
        let iam_auth: Option<CrdIamAuthConfig> = config.iam_auth.as_ref().map(Into::into);
        tracing::debug!(?iam_auth, "Converted IAM auth for CRD");

        let spec = BranchDatabaseSpec {
            id: id.to_owned(),
            database_name: config.database.name.clone(),
            connection_source,
            target: session_target.clone(),
            ttl_secs: config.base.resolved_ttl_secs(),
            version: config.pod.version.clone(),
            image: config.pod.image.clone(),
            profile: config.base.profile.clone(),
            postgres_options: Some(PostgresOptions {
                copy: SqlBranchCopyConfig::from(config.copy.clone()),
                iam_auth,
                connection_settings: config.connection_settings.clone(),
                query_params: config.query_params.clone(),
                additional_databases,
            }),
            mysql_options: None,
            mariadb_options: None,
            dynamodb_options: None,
            mongodb_options: None,
            mssql_options: None,
            redis_options: None,
            spanner_options: None,
            clickhouse_options: None,
            cockroachdb_options: None,
            s3_options: None,
            turbopuffer_options: None,
            generic_options: None,
            migrations,
        };
        let labels = BTreeMap::from([(labels::MIRRORD_BRANCH_ID_LABEL.to_owned(), id.to_owned())]);
        Self {
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
        target_namespace: &str,
        session_target: &KubeResourceTarget,
        literal_values: HashMap<String, String>,
        migrations: Option<MigrationsSpec>,
    ) -> Self {
        let deterministic_name = deterministic_branch_name("mysql", target_namespace, id);
        let connection_source = convert_connection_source(&config.database.connection);
        let iam_auth: Option<CrdIamAuthConfig> = config.iam_auth.as_ref().map(Into::into);
        let spec = BranchDatabaseSpec {
            id: id.to_owned(),
            database_name: config.database.name.clone(),
            connection_source,
            target: session_target.clone(),
            ttl_secs: config.base.resolved_ttl_secs(),
            version: config.pod.version.clone(),
            image: config.pod.image.clone(),
            profile: config.base.profile.clone(),
            postgres_options: None,
            mysql_options: Some(MysqlOptions {
                copy: SqlBranchCopyConfig::from(config.copy.clone()),
                iam_auth,
            }),
            mariadb_options: None,
            dynamodb_options: None,
            mongodb_options: None,
            mssql_options: None,
            redis_options: None,
            spanner_options: None,
            clickhouse_options: None,
            generic_options: None,
            migrations,
            cockroachdb_options: None,
            s3_options: None,
            turbopuffer_options: None,
        };
        let labels = BTreeMap::from([(labels::MIRRORD_BRANCH_ID_LABEL.to_owned(), id.to_owned())]);
        Self {
            deterministic_name,
            labels,
            annotations: BTreeMap::new(),
            spec,
            literal_values,
        }
    }

    pub fn from_mariadb(
        id: &str,
        config: &MariadbBranchConfig,
        target_namespace: &str,
        session_target: &KubeResourceTarget,
        literal_values: HashMap<String, String>,
        migrations: Option<MigrationsSpec>,
    ) -> Self {
        let deterministic_name = deterministic_branch_name("mariadb", target_namespace, id);
        let connection_source = convert_connection_source(&config.database.connection);
        let iam_auth: Option<CrdIamAuthConfig> = config.iam_auth.as_ref().map(Into::into);
        let spec = BranchDatabaseSpec {
            id: id.to_owned(),
            database_name: config.database.name.clone(),
            connection_source,
            target: session_target.clone(),
            ttl_secs: config.base.resolved_ttl_secs(),
            version: config.pod.version.clone(),
            image: config.pod.image.clone(),
            profile: config.base.profile.clone(),
            postgres_options: None,
            mysql_options: None,
            mariadb_options: Some(MariadbOptions {
                copy: SqlBranchCopyConfig::from(config.copy.clone()),
                iam_auth,
            }),
            dynamodb_options: None,
            mongodb_options: None,
            mssql_options: None,
            redis_options: None,
            spanner_options: None,
            clickhouse_options: None,
            cockroachdb_options: None,
            s3_options: None,
            turbopuffer_options: None,
            generic_options: None,
            migrations,
        };
        let labels = BTreeMap::from([(labels::MIRRORD_BRANCH_ID_LABEL.to_owned(), id.to_owned())]);
        Self {
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
        target_namespace: &str,
        session_target: &KubeResourceTarget,
        literal_values: HashMap<String, String>,
    ) -> Self {
        let deterministic_name = deterministic_branch_name("dynamodb", target_namespace, id);
        let connection_source = convert_connection_source(&config.database.connection);
        let spec = BranchDatabaseSpec {
            id: id.to_owned(),
            database_name: config.database.name.clone(),
            connection_source,
            target: session_target.clone(),
            ttl_secs: config.base.resolved_ttl_secs(),
            version: config.pod.version.clone(),
            image: config.pod.image.clone(),
            profile: config.base.profile.clone(),
            postgres_options: None,
            mysql_options: None,
            mariadb_options: None,
            dynamodb_options: Some(DynamodbOptions {
                copy: config.copy.clone().into(),
                iam_auth: config.iam_auth.as_ref().map(Into::into),
            }),
            mongodb_options: None,
            mssql_options: None,
            redis_options: None,
            spanner_options: None,
            clickhouse_options: None,
            cockroachdb_options: None,
            s3_options: None,
            turbopuffer_options: None,
            generic_options: None,
            migrations: None,
        };
        let labels = BTreeMap::from([(labels::MIRRORD_BRANCH_ID_LABEL.to_owned(), id.to_owned())]);
        Self {
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
        target_namespace: &str,
        session_target: &KubeResourceTarget,
        literal_values: HashMap<String, String>,
        migrations: Option<MigrationsSpec>,
    ) -> Self {
        let deterministic_name = deterministic_branch_name("mongodb", target_namespace, id);
        let connection_source = convert_connection_source(&config.database.connection);
        let iam_auth: Option<CrdIamAuthConfig> = config.iam_auth.as_ref().map(Into::into);
        let spec = BranchDatabaseSpec {
            id: id.to_owned(),
            database_name: config.database.name.clone(),
            connection_source,
            target: session_target.clone(),
            ttl_secs: config.base.resolved_ttl_secs(),
            version: config.pod.version.clone(),
            image: config.pod.image.clone(),
            profile: config.base.profile.clone(),
            postgres_options: None,
            mysql_options: None,
            mariadb_options: None,
            dynamodb_options: None,
            mongodb_options: Some(MongodbOptions {
                copy: config.copy.clone().into(),
                iam_auth,
            }),
            mssql_options: None,
            redis_options: None,
            spanner_options: None,
            clickhouse_options: None,
            cockroachdb_options: None,
            s3_options: None,
            turbopuffer_options: None,
            generic_options: None,
            migrations,
        };
        let labels = BTreeMap::from([(labels::MIRRORD_BRANCH_ID_LABEL.to_owned(), id.to_owned())]);
        Self {
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
        target_namespace: &str,
        session_target: &KubeResourceTarget,
        literal_values: HashMap<String, String>,
        migrations: Option<MigrationsSpec>,
    ) -> Self {
        let deterministic_name = deterministic_branch_name("mssql", target_namespace, id);
        let connection_source = convert_connection_source(&config.database.connection);
        let spec = BranchDatabaseSpec {
            id: id.to_owned(),
            database_name: config.database.name.clone(),
            connection_source,
            target: session_target.clone(),
            ttl_secs: config.base.resolved_ttl_secs(),
            version: config.pod.version.clone(),
            image: config.pod.image.clone(),
            profile: config.base.profile.clone(),
            postgres_options: None,
            mysql_options: None,
            mariadb_options: None,
            dynamodb_options: None,
            mongodb_options: None,
            mssql_options: Some(MssqlOptions {
                copy: config.copy.clone().into(),
            }),
            redis_options: None,
            spanner_options: None,
            clickhouse_options: None,
            cockroachdb_options: None,
            s3_options: None,
            turbopuffer_options: None,
            generic_options: None,
            migrations,
        };
        let labels = BTreeMap::from([(labels::MIRRORD_BRANCH_ID_LABEL.to_owned(), id.to_owned())]);
        Self {
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
        target_namespace: &str,
        session_target: &KubeResourceTarget,
        literal_values: HashMap<String, String>,
        migrations: Option<MigrationsSpec>,
    ) -> Self {
        let deterministic_name = deterministic_branch_name("redis", target_namespace, id);
        let connection_source = convert_connection_source(&config.database.connection);
        let spec = BranchDatabaseSpec {
            id: id.to_owned(),
            database_name: config.database.name.clone(),
            connection_source,
            target: session_target.clone(),
            ttl_secs: config.base.resolved_ttl_secs(),
            version: config.pod.version.clone(),
            image: config.pod.image.clone(),
            profile: config.base.profile.clone(),
            postgres_options: None,
            mysql_options: None,
            mariadb_options: None,
            dynamodb_options: None,
            mongodb_options: None,
            mssql_options: None,
            redis_options: Some(RedisOptions {
                copy: config.copy.clone().into(),
            }),
            clickhouse_options: None,
            cockroachdb_options: None,
            s3_options: None,
            turbopuffer_options: None,
            generic_options: None,
            spanner_options: None,
            migrations,
        };
        let labels = BTreeMap::from([(labels::MIRRORD_BRANCH_ID_LABEL.to_owned(), id.to_owned())]);
        Self {
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
        target_namespace: &str,
        session_target: &KubeResourceTarget,
        literal_values: HashMap<String, String>,
    ) -> Self {
        let deterministic_name = deterministic_branch_name("clickhouse", target_namespace, id);
        let connection_source = convert_connection_source(&config.database.connection);
        let spec = BranchDatabaseSpec {
            id: id.to_owned(),
            database_name: config.database.name.clone(),
            connection_source,
            target: session_target.clone(),
            ttl_secs: config.base.resolved_ttl_secs(),
            version: config.pod.version.clone(),
            image: config.pod.image.clone(),
            profile: config.base.profile.clone(),
            postgres_options: None,
            mysql_options: None,
            mariadb_options: None,
            dynamodb_options: None,
            mongodb_options: None,
            mssql_options: None,
            redis_options: None,
            clickhouse_options: Some(ClickhouseOptions {
                copy: config.copy.clone().into(),
            }),
            cockroachdb_options: None,
            s3_options: None,
            turbopuffer_options: None,
            migrations: None,
            spanner_options: None,
            generic_options: None,
        };
        let labels = BTreeMap::from([(labels::MIRRORD_BRANCH_ID_LABEL.to_owned(), id.to_owned())]);
        Self {
            deterministic_name,
            labels,
            annotations: BTreeMap::new(),
            spec,
            literal_values,
        }
    }

    pub fn from_cockroachdb(
        id: &str,
        config: &CockroachdbBranchConfig,
        target_namespace: &str,
        session_target: &KubeResourceTarget,
        literal_values: HashMap<String, String>,
        migrations: Option<MigrationsSpec>,
    ) -> Self {
        let deterministic_name = deterministic_branch_name("cockroachdb", target_namespace, id);
        let connection_source = convert_connection_source(&config.database.connection);
        let spec = BranchDatabaseSpec {
            id: id.to_owned(),
            database_name: config.database.name.clone(),
            connection_source,
            target: session_target.clone(),
            ttl_secs: config.base.resolved_ttl_secs(),
            version: config.pod.version.clone(),
            postgres_options: None,
            mysql_options: None,
            dynamodb_options: None,
            mongodb_options: None,
            mssql_options: None,
            redis_options: None,
            spanner_options: None,
            clickhouse_options: None,
            cockroachdb_options: Some(CockroachdbOptions {
                copy: SqlBranchCopyConfig::from(config.copy.clone()),
            }),
            s3_options: None,
            turbopuffer_options: None,
            generic_options: None,
            mariadb_options: None,
            image: config.pod.image.clone(),
            profile: config.base.profile.clone(),
            migrations,
        };
        let labels = BTreeMap::from([(labels::MIRRORD_BRANCH_ID_LABEL.to_owned(), id.to_owned())]);
        Self {
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
        target_namespace: &str,
        session_target: &KubeResourceTarget,
        literal_values: HashMap<String, String>,
    ) -> Self {
        let deterministic_name = deterministic_branch_name("spanner", target_namespace, id);

        // Spanner keeps the app's project/instance/database untouched (only SPANNER_EMULATOR_HOST
        // is injected), so its source identifiers live flat in `connection.params` under the
        // `project` / `instance` / `database_id` keys rather than the fixed slots, which would
        // trigger a generic connection override. The shared converter carries those flattened keys
        // through to the CRD's `extra`; the operator validates them against SpannerParam and
        // resolves each from the target pod so the init sidecar can recreate and copy them.
        let connection_source = convert_connection_source(&config.database.connection);

        let spec = BranchDatabaseSpec {
            id: id.to_owned(),
            database_name: config.database.name.clone(),
            connection_source,
            target: session_target.clone(),
            ttl_secs: config.base.resolved_ttl_secs(),
            version: config.pod.version.clone(),
            image: config.pod.image.clone(),
            profile: config.base.profile.clone(),
            postgres_options: None,
            mysql_options: None,
            mariadb_options: None,
            dynamodb_options: None,
            mongodb_options: None,
            mssql_options: None,
            redis_options: None,
            clickhouse_options: None,
            cockroachdb_options: None,
            s3_options: None,
            turbopuffer_options: None,
            generic_options: None,
            spanner_options: Some(SpannerOptions {
                copy: config.copy.clone().into(),
                emulator_host_var: Some(config.emulator_host.clone()),
            }),
            migrations: None,
        };
        let labels = BTreeMap::from([(labels::MIRRORD_BRANCH_ID_LABEL.to_owned(), id.to_owned())]);
        Self {
            deterministic_name,
            labels,
            annotations: BTreeMap::new(),
            spec,
            literal_values,
        }
    }

    pub fn from_generic(
        id: &str,
        config: &GenericBranchConfig,
        target_namespace: &str,
        session_target: &KubeResourceTarget,
        literal_values: HashMap<String, String>,
    ) -> Self {
        let deterministic_name = deterministic_branch_name("generic", target_namespace, id);

        // Custom `extra` params flow through the shared converter into the CRD's `extra`, just
        // like Spanner's locators. The operator injects every resolved param into the branch
        // container as `MIRRORD_PARAM_<NAME>` and only redirects the app's host/port vars.
        let connection_source = convert_connection_source(&config.database.connection);

        // The CRD spec is Probe-shaped (optional exec/httpGet); an explicit `tcp` config is
        // the same as no readiness config at all - the operator defaults to TCP on `port`.
        let readiness = config.readiness.as_ref().and_then(|probe| match probe {
            GenericReadinessConfig::Tcp => None,
            GenericReadinessConfig::Exec { command } => Some(GenericReadinessSpec {
                exec: Some(GenericExecProbeSpec {
                    command: command.clone(),
                }),
                http_get: None,
            }),
            GenericReadinessConfig::HttpGet { path, port } => Some(GenericReadinessSpec {
                exec: None,
                http_get: Some(GenericHttpGetProbeSpec {
                    path: path.clone(),
                    port: *port,
                }),
            }),
        });

        let spec = BranchDatabaseSpec {
            id: id.to_owned(),
            database_name: config.database.name.clone(),
            connection_source,
            target: session_target.clone(),
            ttl_secs: config.base.resolved_ttl_secs(),
            // `version` is rejected for generic branches by config verification; the image tag
            // lives in `image`, which a generic branch carries in `genericOptions` (where it is
            // required) rather than in the optional spec-level field.
            version: None,
            image: None,
            profile: config.base.profile.clone(),
            postgres_options: None,
            mysql_options: None,
            mariadb_options: None,
            dynamodb_options: None,
            mongodb_options: None,
            mssql_options: None,
            redis_options: None,
            spanner_options: None,
            clickhouse_options: None,
            cockroachdb_options: None,
            s3_options: None,
            turbopuffer_options: None,
            generic_options: Some(GenericOptions {
                // May be None when `profile` is set; the operator resolves them from the
                // profile's `dbPod.branch` and fails the branch if neither supplies a value.
                image: config.pod.image.clone(),
                port: config.port,
                command: config.command.clone(),
                args: config.args.clone(),
                env: config.env.clone(),
                readiness,
                copy: config.copy.as_ref().map(|copy| GenericCopySpec {
                    image: copy.image.clone(),
                    command: copy.command.clone(),
                    args: copy.args.clone(),
                }),
            }),
            migrations: None,
        };
        let labels = BTreeMap::from([(labels::MIRRORD_BRANCH_ID_LABEL.to_owned(), id.to_owned())]);
        Self {
            deterministic_name,
            labels,
            annotations: BTreeMap::new(),
            spec,
            literal_values,
        }
    }

    /// The branch bucket is created and seeded through the provider's API, with no pod in the
    /// cluster, so the spec carries no image, version, database name or migrations. The bucket
    /// param rides through the shared converter into the CRD's `extra`, where the operator
    /// validates it against `S3Param` and resolves it from the target.
    pub fn from_s3(
        id: &str,
        config: &S3BranchConfig,
        target_namespace: &str,
        session_target: &KubeResourceTarget,
        literal_values: HashMap<String, String>,
    ) -> Self {
        let deterministic_name = deterministic_branch_name("s3", target_namespace, id);
        let connection_source =
            CrdConnectionSource::Params(Box::new(ConnectionParamsSpec::from(&config.source)));

        let spec = BranchDatabaseSpec {
            id: id.to_owned(),
            database_name: None,
            connection_source,
            target: session_target.clone(),
            ttl_secs: config.base.resolved_ttl_secs(),
            version: None,
            image: None,
            profile: config.base.profile.clone(),
            postgres_options: None,
            mysql_options: None,
            mariadb_options: None,
            dynamodb_options: None,
            mongodb_options: None,
            mssql_options: None,
            redis_options: None,
            spanner_options: None,
            clickhouse_options: None,
            cockroachdb_options: None,
            s3_options: Some(S3Options {
                provider: config.provider.into(),
                copy: config.copy.clone().into(),
            }),
            turbopuffer_options: None,
            generic_options: None,
            migrations: None,
        };
        let labels = BTreeMap::from([(labels::MIRRORD_BRANCH_ID_LABEL.to_owned(), id.to_owned())]);
        Self {
            deterministic_name,
            labels,
            annotations: BTreeMap::new(),
            spec,
            literal_values,
        }
    }

    /// The branch namespace is cloned through turbopuffer's API, with no pod in the cluster,
    /// so the spec carries no image, version, database name or migrations. The namespace,
    /// API key and endpoint params ride through the shared converter into the CRD's `extra`,
    /// where the operator validates them against `TurbopufferParam` and resolves them from
    /// the target.
    pub fn from_turbopuffer(
        id: &str,
        config: &TurbopufferBranchConfig,
        target_namespace: &str,
        session_target: &KubeResourceTarget,
        literal_values: HashMap<String, String>,
    ) -> Self {
        let deterministic_name = deterministic_branch_name("turbopuffer", target_namespace, id);
        let connection_source =
            CrdConnectionSource::Params(Box::new(ConnectionParamsSpec::from(&config.source)));

        let spec = BranchDatabaseSpec {
            id: id.to_owned(),
            database_name: None,
            connection_source,
            target: session_target.clone(),
            ttl_secs: config.base.resolved_ttl_secs(),
            version: None,
            image: None,
            profile: config.base.profile.clone(),
            postgres_options: None,
            mysql_options: None,
            mariadb_options: None,
            dynamodb_options: None,
            mongodb_options: None,
            mssql_options: None,
            redis_options: None,
            spanner_options: None,
            clickhouse_options: None,
            cockroachdb_options: None,
            s3_options: None,
            turbopuffer_options: Some(TurbopufferOptions {
                copy: config.copy.clone().into(),
            }),
            generic_options: None,
            migrations: None,
        };
        let labels = BTreeMap::from([(labels::MIRRORD_BRANCH_ID_LABEL.to_owned(), id.to_owned())]);
        Self {
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
    use std::{
        collections::{BTreeMap, HashMap},
        time::Duration,
    };

    use http::{Method, Request, Response};
    use k8s_openapi::{
        apimachinery::pkg::apis::meta::v1::{Condition, MicroTime, Time},
        jiff::Timestamp,
    };
    use kube::{Api, Client, ResourceExt, client::Body};
    use mirrord_config::{
        feature::database_branches::{
            S3BranchConfig, SingleOrVec, SqlBranchMigrationsConfig, TurbopufferBranchConfig,
        },
        target::Target,
    };
    use mirrord_progress::NullProgress;

    use super::{
        BranchDatabase, BranchDatabaseId, ConfigConnectionSource, ConnectionParamsSpec,
        CrdConnectionSource, DatabaseBranchesConfig, MigrationsSpec, ObjectMeta, OperatorApiError,
        SOURCE_COMPATIBLE_CONDITION, UnifiedBranchParams, UnifiedDatabaseBranchParams,
        build_migration_archive, classify_existing_branches, convert_connection_source,
        create_branches, extract_literal_values, read_migrations,
        replace_spec_values_with_secret_refs, replace_values_with_secret_refs, resolve_branch_id,
        reused_branch_connection_sources,
    };
    use crate::crd::{
        db_branching::{
            branch_database::{
                DialectConfig, S3BranchCopyMode, S3Provider, TurbopufferBranchCopyMode,
            },
            core::{BranchDatabasePhase, BranchDatabaseStatus, ConnectionSourceKind},
        },
        session::KubeResourceTarget,
    };

    /// Builds the unified params of a config holding `branches`, with session key
    /// `session-key`.
    fn branches_params(
        branches: serde_json::Value,
    ) -> Result<UnifiedDatabaseBranchParams, OperatorApiError> {
        let mut config: DatabaseBranchesConfig = serde_json::from_value(branches).unwrap();
        let target = "deployment/my-app".parse::<Target>().unwrap();
        UnifiedDatabaseBranchParams::new(
            &mut config,
            &target,
            "default",
            "session-key",
            &NullProgress,
        )
    }

    /// Builds the unified params of a config holding one pg branch.
    fn pg_branch_params(branch: serde_json::Value) -> UnifiedBranchParams {
        let (_, params) = branches_params(serde_json::json!([branch]))
            .unwrap()
            .branches
            .drain()
            .next()
            .expect("one branch configured");
        params
    }

    /// The params of an S3 branch with the given `id`.
    fn s3_branch_params(id: &str) -> UnifiedBranchParams {
        let config: S3BranchConfig = serde_json::from_value(serde_json::json!({
            "source": { "type": "env_from", "params": { "bucket": "MY_BUCKET_ENV_VAR" } },
        }))
        .unwrap();
        let session_target = KubeResourceTarget {
            api_version: "apps/v1".to_owned(),
            kind: "Deployment".to_owned(),
            name: "my-app".to_owned(),
            container: String::new(),
        };
        UnifiedBranchParams::from_s3(id, &config, "default", &session_target, HashMap::new())
    }

    /// A branch found under the deterministic name for `id`, in the given phase (`None` is a
    /// branch the operator has not picked up yet).
    fn found_branch(id: &str, phase: Option<BranchDatabasePhase>) -> (String, BranchDatabase) {
        let params = s3_branch_params(id);
        let status = phase.map(|phase| BranchDatabaseStatus {
            pod_name: None,
            phase,
            expire_time: MicroTime(Timestamp::now()),
            session_info: HashMap::new(),
            error: None,
            migrations: None,
            copy: None,
            conditions: Vec::new(),
            source: None,
        });
        let branch = BranchDatabase {
            metadata: ObjectMeta {
                name: Some(params.deterministic_name),
                ..Default::default()
            },
            spec: params.spec,
            status,
        };
        (branch.name_any(), branch)
    }

    /// A branch this session creates reaches the caller as it was once ready, so the conditions
    /// the operator recorded on the way, such as a `SourceCompatible` warning, are there to relay.
    #[tokio::test]
    async fn created_branch_comes_back_as_it_was_once_ready() {
        let (service, mut handle) = tower_test::mock::pair::<Request<Body>, Response<Body>>();
        let api = Api::<BranchDatabase>::namespaced(Client::new(service, "default"), "default");

        let (name, created) = found_branch("created", None);
        let (_, mut ready) = found_branch("created", Some(BranchDatabasePhase::Ready));
        let warning = Condition {
            last_transition_time: Time(Timestamp::now()),
            message: "the branch is missing stored routines".to_owned(),
            observed_generation: None,
            reason: "SourceMismatch".to_owned(),
            status: "False".to_owned(),
            type_: SOURCE_COMPATIBLE_CONDITION.to_owned(),
        };
        ready
            .status
            .as_mut()
            .unwrap()
            .conditions
            .push(warning.clone());

        let json =
            |value: serde_json::Value| Response::new(Body::from(value.to_string().into_bytes()));

        let (outcome, ()) = tokio::join!(
            create_branches(
                &api,
                HashMap::from([(name.clone(), s3_branch_params("created"))]),
                Duration::from_secs(10),
                &NullProgress,
            ),
            async {
                let (request, send) = handle.next_request().await.unwrap();
                assert_eq!(request.method(), Method::POST);
                send.send_response(json(serde_json::to_value(&created).unwrap()));

                let (request, send) = handle.next_request().await.unwrap();
                assert_eq!(request.method(), Method::GET);
                send.send_response(json(serde_json::json!({
                    "apiVersion": "dbs.mirrord.metalbear.co/v1alpha1",
                    "kind": "BranchDatabaseList",
                    "metadata": { "resourceVersion": "1" },
                    "items": [ready],
                })));
            },
        );

        let outcome = outcome.unwrap();
        let conditions = outcome
            .created
            .get(&name)
            .unwrap()
            .status
            .as_ref()
            .unwrap()
            .conditions
            .iter()
            .map(|condition| (condition.type_.as_str(), condition.message.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            conditions,
            [(warning.type_.as_str(), warning.message.as_str())]
        );
    }

    /// The lookup runs before the create, so what it reports has to be what the create
    /// would run into: a Ready branch is reused as is, anything still on its way up
    /// (no status yet, Init, Pending, or a phase this build does not know) is waited on,
    /// and a Failed branch is neither - it holds the name and has to be surfaced, not
    /// created over. Reporting "0 ready, 0 pending" for a branch in any of these states
    /// is what let the create's 409 reuse look like a fresh branch.
    #[test]
    fn every_found_branch_lands_in_exactly_one_bucket_by_phase() {
        let existing = classify_existing_branches([
            found_branch("ready", Some(BranchDatabasePhase::Ready)),
            found_branch("no-status", None),
            found_branch("init", Some(BranchDatabasePhase::Init)),
            found_branch("pending", Some(BranchDatabasePhase::Pending)),
            found_branch("unknown", Some(BranchDatabasePhase::Unknown)),
            found_branch("failed", Some(BranchDatabasePhase::Failed)),
        ]);

        let ids = |bucket: &HashMap<String, BranchDatabase>| {
            let mut ids = bucket
                .values()
                .map(|branch| branch.spec.id.clone())
                .collect::<Vec<_>>();
            ids.sort();
            ids
        };
        assert_eq!(ids(&existing.ready), ["ready"]);
        assert_eq!(
            ids(&existing.pending),
            ["init", "no-status", "pending", "unknown"]
        );
        assert_eq!(ids(&existing.failed), ["failed"]);
    }

    /// The lookup keeps the key the caller asked with, so the caller can subtract the result
    /// from its create list by the same key it built it with.
    #[test]
    fn found_branches_keep_the_requested_key() {
        let (key, branch) = found_branch("shared-id", Some(BranchDatabasePhase::Ready));
        let existing = classify_existing_branches([(key.clone(), branch)]);
        assert!(existing.ready.contains_key(&key));
    }

    /// A `users` workload reusing the branch an `orders` workload created under the same `id`
    /// declared `AUDIT_DB_*` vars, but the branch spec still says `DB_*`. Building the session
    /// env from the branch spec then appended the creator's `DB_*` vars to the `users` pod and
    /// left `AUDIT_DB_*` pointing at the source database. The session has to ship its own
    /// mapping for that branch - and only for that one: a reused branch whose spec already
    /// matches, or a branch this session did not ask for, sends nothing.
    #[test]
    fn reused_branch_ships_the_sessions_mapping_only_when_it_differs() {
        let (same_id, same_branch) = found_branch("same", Some(BranchDatabasePhase::Ready));
        let (other_id, other_branch) = found_branch("other", Some(BranchDatabasePhase::Ready));
        let (unrequested_id, unrequested_branch) =
            found_branch("unrequested", Some(BranchDatabasePhase::Ready));

        let audit_source = CrdConnectionSource::Params(Box::new(ConnectionParamsSpec {
            host: Some(SingleOrVec::from(ConnectionSourceKind::Env {
                container: None,
                variable: "AUDIT_DB_HOST".to_owned(),
            })),
            ..Default::default()
        }));
        let requested = HashMap::from([
            (same_id.clone(), same_branch.spec.connection_source.clone()),
            (other_id.clone(), audit_source.clone()),
        ]);

        let shipped = reused_branch_connection_sources(
            &requested,
            [
                (&same_id, &same_branch),
                (&other_id, &other_branch),
                (&unrequested_id, &unrequested_branch),
            ],
        );

        assert_eq!(
            shipped,
            BTreeMap::from([(other_branch.name_any(), audit_source)]),
            "only the branch whose spec names other vars carries the session's mapping"
        );
    }

    /// An S3 branch is cloned in the provider's cloud, so its spec carries none of the pod
    /// fields, and its bucket rides into the CRD's `extra` for the operator to resolve from
    /// the target - where it has to survive the dialect's extra-param validation.
    #[test]
    fn s3_spec_has_no_pod_fields_and_carries_the_bucket_param() {
        let config: S3BranchConfig = serde_json::from_value(serde_json::json!({
            "source": { "type": "env_from", "params": { "bucket": "MY_BUCKET_ENV_VAR" } },
            "copy": { "mode": "all", "objects": ["^fixtures/.*"] },
        }))
        .unwrap();
        let session_target = KubeResourceTarget {
            api_version: "apps/v1".to_owned(),
            kind: "Deployment".to_owned(),
            name: "my-app".to_owned(),
            container: String::new(),
        };

        let params = UnifiedBranchParams::from_s3(
            "my-branch",
            &config,
            "default",
            &session_target,
            HashMap::new(),
        );

        assert_eq!(params.spec.version, None);
        assert_eq!(params.spec.image, None);
        assert_eq!(params.spec.database_name, None);
        assert!(params.spec.migrations.is_none());

        let options = params.spec.s3_options.as_ref().expect("built above");
        assert_eq!(options.provider, S3Provider::Aws);
        assert!(matches!(options.copy.mode, S3BranchCopyMode::All));
        assert_eq!(options.copy.objects, ["^fixtures/.*".to_owned()].as_slice(),);

        let CrdConnectionSource::Params(source) = &params.spec.connection_source else {
            panic!("an S3 source is always params-shaped");
        };
        assert!(matches!(
            source.extra.get("bucket").and_then(|kinds| kinds.first()),
            Some(ConnectionSourceKind::EnvFrom { variable, .. }) if variable == "MY_BUCKET_ENV_VAR"
        ));

        assert!(matches!(params.spec.dialect(), Ok(DialectConfig::S3(_))));
    }

    /// A turbopuffer branch is cloned by turbopuffer itself, so its spec carries none of the
    /// pod fields, and its namespace, API key and endpoint ride into the CRD's `extra` for
    /// the operator to resolve from the target - where they have to survive the dialect's
    /// extra-param validation.
    #[test]
    fn turbopuffer_spec_has_no_pod_fields_and_carries_its_params() {
        let config: TurbopufferBranchConfig = serde_json::from_value(serde_json::json!({
            "source": {
                "params": {
                    "namespace": "TPUF_NAMESPACE",
                    "api_key": { "secret": "turbopuffer", "key": "api-key" },
                    "base_url": "TURBOPUFFER_BASE_URL",
                },
            },
            "copy": { "mode": "all" },
        }))
        .unwrap();
        let session_target = KubeResourceTarget {
            api_version: "apps/v1".to_owned(),
            kind: "Deployment".to_owned(),
            name: "my-app".to_owned(),
            container: String::new(),
        };

        let params = UnifiedBranchParams::from_turbopuffer(
            "my-branch",
            &config,
            "default",
            &session_target,
            HashMap::new(),
        );

        assert_eq!(params.spec.version, None);
        assert_eq!(params.spec.image, None);
        assert_eq!(params.spec.database_name, None);
        assert!(params.spec.migrations.is_none());

        let options = params
            .spec
            .turbopuffer_options
            .as_ref()
            .expect("built above");
        assert!(matches!(options.copy.mode, TurbopufferBranchCopyMode::All));

        let CrdConnectionSource::Params(source) = &params.spec.connection_source else {
            panic!("a turbopuffer source is always params-shaped");
        };
        assert!(matches!(
            source.extra.get("namespace").and_then(|kinds| kinds.first()),
            Some(ConnectionSourceKind::Env { variable, .. }) if variable == "TPUF_NAMESPACE"
        ));
        assert!(matches!(
            source.extra.get("api_key").and_then(|kinds| kinds.first()),
            Some(ConnectionSourceKind::Secret { name, key, .. }) if name == "turbopuffer" && key == "api-key"
        ));
        assert!(source.extra.contains_key("base_url"));

        assert!(matches!(
            params.spec.dialect(),
            Ok(DialectConfig::Turbopuffer(_))
        ));
    }

    /// Literal `value` fields in custom `extra` params must be extracted into the credential
    /// Secret exactly like the fixed slots - otherwise they ship in the CRD in plaintext.
    /// Generic branches lean on extras for credentials; this also covers the latent Spanner
    /// case.
    #[test]
    fn literal_values_in_extras_are_extracted_and_replaced() {
        let mut source: ConfigConnectionSource = serde_json::from_value(serde_json::json!({
            "params": {
                "host": "DB_HOST",
                "password": { "env_var_name": "DB_PASSWORD", "value": "hunter2" },
                "token": { "env_var_name": "SERVICE_TOKEN", "value": "tok-123" },
                "org": "SERVICE_ORG"
            }
        }))
        .unwrap();

        let mut literal_values = std::collections::HashMap::new();
        extract_literal_values(&mut source, &mut literal_values);

        assert_eq!(
            literal_values,
            std::collections::HashMap::from([
                ("DB_PASSWORD".to_owned(), "hunter2".to_owned()),
                ("SERVICE_TOKEN".to_owned(), "tok-123".to_owned()),
            ]),
        );

        let mut crd_source = convert_connection_source(&source);
        replace_values_with_secret_refs(&mut crd_source, "creds-secret", &literal_values);

        let CrdConnectionSource::Params(params) = &crd_source else {
            panic!("expected params mode");
        };

        use crate::crd::db_branching::core::ConnectionSourceKind;
        let token_kind = params.extra.get("token").unwrap().first().unwrap();
        assert!(
            matches!(
                token_kind,
                ConnectionSourceKind::Secret {
                    name,
                    key,
                    env_var_name: Some(env_var_name),
                } if name == "creds-secret"
                    && key == "SERVICE_TOKEN"
                    && env_var_name == "SERVICE_TOKEN"
            ),
            "literal extra should become a Secret ref, got {token_kind:?}",
        );
        // A plain env-var extra stays untouched (Spanner's locators are this shape).
        let org_kind = params.extra.get("org").unwrap().first().unwrap();
        assert!(
            matches!(org_kind, ConnectionSourceKind::Env { variable, .. } if variable == "SERVICE_ORG"),
        );
    }

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

    /// A local `path` uploads an archive; the in-image `locations` field stays empty so the
    /// operator takes the archive path.
    #[test]
    fn read_migrations_local_flyway_uploads_archive() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("V1__a.sql"), b"create table a ();").unwrap();

        let config = SqlBranchMigrationsConfig::Flyway {
            path: Some(dir.path().to_owned()),
            image: None,
            locations: vec![],
        };

        let Some(MigrationsSpec::Flyway {
            image,
            archive,
            locations,
        }) = read_migrations(Some(&config)).unwrap()
        else {
            panic!("expected a flyway spec");
        };
        assert!(image.is_none());
        assert!(archive.is_some(), "local path must upload an archive");
        assert!(locations.is_empty());
    }

    /// Image-native flyway ships nothing: no archive is built, the image and locations pass
    /// through for the operator to run in-image.
    #[test]
    fn read_migrations_image_native_flyway_uploads_nothing() {
        let config = SqlBranchMigrationsConfig::Flyway {
            path: None,
            image: Some("example.com/migrations:1".to_owned()),
            locations: vec!["filesystem:/flyway/sql".to_owned()],
        };

        let Some(MigrationsSpec::Flyway {
            image,
            archive,
            locations,
        }) = read_migrations(Some(&config)).unwrap()
        else {
            panic!("expected a flyway spec");
        };
        assert_eq!(image.as_deref(), Some("example.com/migrations:1"));
        assert!(archive.is_none(), "in-image migrations must not upload");
        assert_eq!(locations, vec!["filesystem:/flyway/sql".to_owned()]);
    }

    /// The container flavor passes the user's image/command/env through unchanged.
    #[test]
    fn read_migrations_container_passes_through() {
        let config = SqlBranchMigrationsConfig::Container {
            image: "example.com/app:1".to_owned(),
            command: Some(vec!["./db_setup.sh".to_owned()]),
            args: None,
            env: BTreeMap::from([("SNAPSHOT_JOB".to_owned(), "true".to_owned())]),
        };

        let Some(MigrationsSpec::Container {
            image,
            command,
            args,
            env,
        }) = read_migrations(Some(&config)).unwrap()
        else {
            panic!("expected a container spec");
        };
        assert_eq!(image, "example.com/app:1");
        assert_eq!(command, Some(vec!["./db_setup.sh".to_owned()]));
        assert!(args.is_none());
        assert_eq!(
            env,
            BTreeMap::from([("SNAPSHOT_JOB".to_owned(), "true".to_owned())])
        );
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

    /// The additional databases reach the CRD with their own name, copy mode and connection,
    /// and a literal value in an additional connection lands in the credential Secret like
    /// one in the branch's own connection.
    #[test]
    fn pg_spec_carries_additional_databases_and_moves_their_literals_to_the_secret() {
        let mut params = pg_branch_params(serde_json::json!({
            "id": "shared",
            "type": "pg",
            "connection": { "url": { "type": "env", "variable": "DATABASE_URL" } },
            "copy": { "mode": "all" },
            "additional_databases": [
                {
                    "name": "analytics",
                    "connection": { "params": {
                        "host": "ANALYTICS_HOST",
                        "password": { "env_var_name": "ANALYTICS_PASSWORD", "value": "hunter2" },
                        "database": "ANALYTICS_DB"
                    } },
                    "copy": { "mode": "schema", "tables": { "events": { "filter": "id < 10" } } }
                },
                { "name": "audit" }
            ]
        }));

        let options = params.spec.postgres_options.as_ref().expect("a pg branch");
        let [analytics, audit] = options.additional_databases.as_slice() else {
            panic!("expected two additional databases, got {options:?}");
        };
        assert_eq!(analytics.name, "analytics");
        assert_eq!(analytics.copy.mode.as_ref(), "schema");
        assert!(
            analytics
                .copy
                .items
                .as_ref()
                .is_some_and(|items| items.contains_key("events"))
        );
        assert!(analytics.connection_source.is_some());
        assert_eq!(audit.name, "audit");
        assert_eq!(audit.copy.mode.as_ref(), "empty");
        assert!(audit.connection_source.is_none());
        assert!(matches!(
            params.spec.dialect(),
            Ok(DialectConfig::Postgres(_))
        ));

        assert_eq!(
            params.literal_values,
            HashMap::from([("ANALYTICS_PASSWORD".to_owned(), "hunter2".to_owned())])
        );
        let literal_values = params.literal_values.clone();
        replace_spec_values_with_secret_refs(&mut params.spec, "creds-secret", &literal_values);
        let options = params.spec.postgres_options.as_ref().expect("a pg branch");
        let Some(CrdConnectionSource::Params(analytics_source)) = options
            .additional_databases
            .first()
            .and_then(|database| database.connection_source.as_ref())
        else {
            panic!("the analytics connection is params-shaped");
        };
        assert!(matches!(
            analytics_source.password.as_ref().and_then(|kinds| kinds.first()),
            Some(ConnectionSourceKind::Secret { name, key, .. })
                if name == "creds-secret" && key == "ANALYTICS_PASSWORD"
        ));
    }

    /// Builds the params of a pg branch with id `shared` and the given additional databases.
    fn pg_params_with(additional: serde_json::Value) -> UnifiedBranchParams {
        pg_branch_params(serde_json::json!({
            "id": "shared",
            "type": "pg",
            "connection": { "url": "DATABASE_URL" },
            "additional_databases": additional
        }))
    }

    /// Reuse goes by resource name, so the additional databases and their connections are
    /// part of it: a branch holding another set, or connecting one of them differently, is
    /// never picked up. A branch without additional databases keeps the name it had before the
    /// field existed, and the order of the list does not matter.
    #[test]
    fn pg_branch_name_depends_on_the_additional_databases() {
        let name = |additional| pg_params_with(additional).deterministic_name;

        let plain = name(serde_json::json!([]));
        assert_eq!(
            plain,
            super::deterministic_branch_name("pg", "default", "shared")
        );

        let two = name(serde_json::json!([{ "name": "a" }, { "name": "b" }]));
        assert_ne!(two, plain);
        assert_eq!(
            two,
            name(serde_json::json!([{ "name": "b" }, { "name": "a" }]))
        );
        assert_ne!(two, name(serde_json::json!([{ "name": "a" }])));

        let connected = name(serde_json::json!([
            { "name": "a", "connection": { "url": "A_URL" } },
            { "name": "b" }
        ]));
        assert_ne!(connected, two);
        assert_eq!(
            connected,
            name(serde_json::json!([
                { "name": "b" },
                { "name": "a", "connection": { "url": "A_URL" } }
            ]))
        );
        assert_ne!(
            connected,
            name(serde_json::json!([
                { "name": "a", "connection": { "url": "OTHER_URL" } },
                { "name": "b" }
            ]))
        );
        assert_eq!(
            connected,
            name(serde_json::json!([
                {
                    "name": "a",
                    "connection": { "url": { "type": "env", "variable": "A_URL" } }
                },
                { "name": "b" }
            ])),
            "two spellings of one connection are the same branch"
        );
    }

    /// Entries without an `id` share the session key, yet each is its own branch, keyed by its
    /// resource name, as long as the type or the additional databases differ.
    #[test]
    fn entries_without_an_id_are_separate_branches() {
        let params = branches_params(serde_json::json!([
            { "type": "pg", "connection": { "url": "DATABASE_URL" } },
            { "type": "mysql", "connection": { "url": "MYSQL_URL" } },
            {
                "type": "pg",
                "connection": { "url": "OTHER_DATABASE_URL" },
                "additional_databases": [{ "name": "analytics" }]
            }
        ]))
        .unwrap();

        assert_eq!(params.branches.len(), 3);
        for (name, branch) in &params.branches {
            assert_eq!(name, &branch.deterministic_name);
            assert_eq!(branch.spec.id, "session-key");
        }
    }

    /// Two entries that would be the very same branch cannot both be served by it, so the
    /// session is refused instead of one of them being dropped.
    #[test]
    fn two_entries_of_one_branch_are_refused() {
        let result = branches_params(serde_json::json!([
            { "type": "pg", "connection": { "url": "DATABASE_URL" } },
            { "type": "pg", "connection": { "url": "OTHER_DATABASE_URL" } }
        ]));

        assert!(
            matches!(result, Err(OperatorApiError::BranchCreationFailed { ref message, .. }) if message.contains("`feature.db_branches[0]` and `feature.db_branches[1]`") && message.contains("neither sets an `id`")),
            "{:?}",
            result.err()
        );
    }
}
