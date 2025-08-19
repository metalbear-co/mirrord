use std::{
    collections::{BTreeMap, HashMap},
    time::Duration,
};

use kube::{
    Api,
    api::{ListParams, ObjectMeta},
    runtime::wait::await_condition,
};
use mirrord_config::{
    feature::database_branches::{
        ConnectionSource, ConnectionSourceKind, DatabaseBranchConfig, DatabaseBranchesConfig,
        DatabaseType,
    },
    target::{Target, TargetDisplay},
};
use mirrord_progress::Progress;
use sha2::{Digest, Sha256};
use tracing::Level;

use crate::{
    client::error::{OperatorApiError, OperatorOperation},
    crd::{
        kube_target::KubeTarget,
        mysql_branching::{
            BranchDatabasePhase, ConnectionSource as CrdConnectionSource,
            ConnectionSourceKind as CrdConnectionSourceKind, MysqlBranchDatabase,
            MysqlBranchDatabaseSpec,
        },
    },
};

const MYSQL_BRANCH_CREATION_TIMEOUT: Duration = Duration::from_secs(10);

/// Create MySQL branch databases and wait for their readiness.
///
/// Timeout after [`MYSQL_BRANCH_CREATION_TIMEOUT`].
#[tracing::instrument(level = Level::TRACE, skip_all, err, ret)]
pub(crate) async fn create_mysql_branches<P: Progress>(
    api: &Api<MysqlBranchDatabase>,
    params: HashMap<BranchDatabaseId, MysqlBranchParams>,
    progress: &P,
) -> Result<HashMap<BranchDatabaseId, MysqlBranchDatabase>, OperatorApiError> {
    let mut subtask = progress.subtask("creating new MySQL branch databases");
    let mut created_branches = HashMap::new();
    let branch_names = params.values().map(|p| p.name.clone()).collect::<Vec<_>>();

    for (id, params) in params {
        let name = params.name;
        let branch = MysqlBranchDatabase {
            metadata: ObjectMeta {
                name: Some(name.clone()),
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

    let ready = branch_names
        .iter()
        .map(|name| {
            await_condition(api.clone(), name, |db: Option<&MysqlBranchDatabase>| {
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
    tokio::time::timeout(
        MYSQL_BRANCH_CREATION_TIMEOUT,
        futures::future::join_all(ready),
    )
    .await
    .map_err(|_| OperatorApiError::OperationTimeout {
        operation: OperatorOperation::MysqlBranching,
    })?;
    subtask.success(Some("new MySQL branch databases ready"));

    Ok(created_branches)
}

/// Given parameters of all MySQL branch databases needed for a session, list reusable ones.
pub(crate) async fn list_reusable_mysql_branches<P: Progress>(
    api: &Api<MysqlBranchDatabase>,
    params: &HashMap<BranchDatabaseId, MysqlBranchParams>,
    progress: &P,
) -> Result<HashMap<BranchDatabaseId, MysqlBranchDatabase>, OperatorApiError> {
    let mut subtask = progress.subtask("listing reusable MySQL branch databases");
    let label_selector = if params.is_empty() {
        None
    } else {
        Some(format!(
            "{} in ({})",
            labels::MIRRORD_MYSQL_BRANCH_ID_LABEL,
            params
                .keys()
                .map(|id| id.as_ref())
                .collect::<Vec<_>>()
                .join(",")
        ))
    };
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
        .map(|db| (BranchDatabaseId(db.spec.id.clone()), db))
        .collect::<HashMap<_, _>>();

    subtask.success(Some(&format!(
        "{} reusable MySQL branches found",
        reusable_mysql_branches.len()
    )));
    Ok(reusable_mysql_branches)
}

pub(crate) struct DatabaseBranchParams {
    pub(crate) mysql: HashMap<BranchDatabaseId, MysqlBranchParams>,
}

impl DatabaseBranchParams {
    /// Create branch database parameters.
    ///
    /// We derive branch database ID from inputs unless the user explicitly specifies it.
    pub(crate) fn new(
        config: &DatabaseBranchesConfig,
        target: &Target,
        namespace: &str,
        client_public_key: &[u8],
    ) -> Self {
        let mut mysql = HashMap::new();
        for branch_db_config in config.0.iter() {
            let id = if let Some(id) = branch_db_config.id.clone() {
                BranchDatabaseId(id)
            } else {
                BranchDatabaseId::new(client_public_key, target, namespace, &branch_db_config.name)
            };
            match branch_db_config._type {
                DatabaseType::MySql => {
                    let params = MysqlBranchParams::new(id.0.as_str(), branch_db_config, target);
                    mysql.insert(id, params)
                }
            };
        }
        Self { mysql }
    }
}

/// Branch database ID derived from user's certificate, target, namespace, and database name
/// in this particular order.
///
/// This Id is used for selecting reusable branch database and should not be confused with
/// Kubernetes resource uid.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct BranchDatabaseId(String);

impl BranchDatabaseId {
    pub(crate) fn new(
        client_public_key: &[u8],
        target: &Target,
        namespace: &str,
        database_name: &str,
    ) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(client_public_key);
        hasher.update(target.to_string());
        hasher.update(namespace);
        hasher.update(database_name);
        let hash = hasher.finalize();
        Self(hex::encode(hash))
    }
}

impl std::fmt::Display for BranchDatabaseId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl AsRef<str> for BranchDatabaseId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone)]
pub(crate) struct MysqlBranchParams {
    pub(crate) name: String,
    pub(crate) labels: BTreeMap<String, String>,
    pub(crate) spec: MysqlBranchDatabaseSpec,
}

impl MysqlBranchParams {
    const MYSQL_BRANCH_DEFAULT_TTL: u64 = 120;

    pub(crate) fn new(id: &str, config: &DatabaseBranchConfig, target: &Target) -> Self {
        let name = format!("{}-mysql-branch-{}", target.name(), &id[..8]);
        let connection_source = match &config.connection {
            ConnectionSource::Url(kind) => match kind {
                ConnectionSourceKind::Env {
                    container,
                    variable,
                } => CrdConnectionSource::Url(CrdConnectionSourceKind::Env {
                    container: container.clone(),
                    variable: variable.clone(),
                }),
            },
        };
        let spec = MysqlBranchDatabaseSpec {
            id: id.to_string(),
            database_name: config.name.clone(),
            connection_source,
            target: KubeTarget::from(target.clone()),
            ttl_secs: config.ttl_secs.unwrap_or(Self::MYSQL_BRANCH_DEFAULT_TTL),
            mysql_version: config.version.clone(),
        };
        let labels = BTreeMap::from([(
            labels::MIRRORD_MYSQL_BRANCH_ID_LABEL.to_string(),
            id.to_string(),
        )]);
        Self { name, labels, spec }
    }
}

pub(crate) mod labels {
    pub(crate) const MIRRORD_MYSQL_BRANCH_ID_LABEL: &str = "mirrord-mysql-branch-id";
}
