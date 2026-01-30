use std::collections::HashSet;

use kube::{
    Api,
    api::{DeleteParams, ListParams},
};
use mirrord_config::{LayerConfig, config::ConfigContext};
use mirrord_operator::crd::{mysql_branching::MysqlBranchDatabase, pg_branching::PgBranchDatabase};
use mirrord_progress::{Progress, ProgressTracker};
use prettytable::{Table, row};

use crate::{
    CliResult,
    config::{DbBranchesArgs, DbBranchesCommand},
    kube::kube_client_from_layer_config,
};

#[derive(Debug)]
struct BranchInfo {
    name: String,
    db_type: &'static str,
    phase: Option<String>,
    ttl: u64,
    database: Option<String>,
    users: Option<String>,
    expire_time: Option<String>,
}

impl From<&MysqlBranchDatabase> for BranchInfo {
    fn from(branch: &MysqlBranchDatabase) -> Self {
        Self {
            name: branch.metadata.name.clone().unwrap_or_default(),
            db_type: "MySQL",
            phase: branch.status.as_ref().map(|s| s.phase.to_string()),
            ttl: branch.spec.ttl_secs,
            database: branch.spec.database_name.clone(),
            users: branch.status.as_ref().and_then(|s| {
                if s.session_info.is_empty() {
                    None
                } else {
                    let mut user_list: Vec<_> = s
                        .session_info
                        .values()
                        .map(|session| session.owner.k8s_username.clone())
                        .collect();
                    user_list.sort();
                    Some(user_list.join("\n"))
                }
            }),
            expire_time: branch.status.as_ref().map(|s| s.expire_time.0.to_rfc3339()),
        }
    }
}

impl From<&PgBranchDatabase> for BranchInfo {
    fn from(branch: &PgBranchDatabase) -> Self {
        Self {
            name: branch.metadata.name.clone().unwrap_or_default(),
            db_type: "PostgreSQL",
            phase: branch.status.as_ref().map(|s| s.phase.to_string()),
            ttl: branch.spec.ttl_secs,
            database: branch.spec.database_name.clone(),
            users: branch.status.as_ref().and_then(|s| {
                if s.session_info.is_empty() {
                    None
                } else {
                    let mut user_list: Vec<_> = s
                        .session_info
                        .values()
                        .map(|session| session.owner.k8s_username.clone())
                        .collect();
                    user_list.sort();
                    Some(user_list.join("\n"))
                }
            }),
            expire_time: branch.status.as_ref().map(|s| s.expire_time.0.to_rfc3339()),
        }
    }
}

pub async fn db_branches_command(args: DbBranchesArgs) -> CliResult<()> {
    match &args.command {
        DbBranchesCommand::Status { names } => status_command(&args, names.as_slice()).await,
        DbBranchesCommand::Destroy { all, names } => destroy_command(&args, *all, names).await,
    }
}

fn get_api<T>(args: &DbBranchesArgs, client: &kube::Client, layer_config: &LayerConfig) -> Api<T> {
    if args.all_namespaces {
        Api::all(client.clone())
    } else if let Some(namespace) = &args.namespace {
        Api::namespaced(client.clone(), namespace)
    } else if let Some(namespace) = &layer_config.target.namespace {
        Api::namespaced(client.clone(), namespace)
    } else {
        Api::default_namespaced(client.clone())
    }
}

async fn status_command(args: &DbBranchesArgs, names: &[String]) -> CliResult<()> {
    let names: HashSet<_> = names.iter().collect();

    let mut progress = ProgressTracker::from_env("DB Branches Status");
    let mut status_progress = progress.subtask("fetching branches");

    let mut cfg_context = ConfigContext::default()
        .override_env_opt(LayerConfig::FILE_PATH_ENV, args.config_file.clone())
        .override_env_opt("MIRRORD_TARGET_NAMESPACE", args.namespace.clone());

    let layer_config = LayerConfig::resolve(&mut cfg_context)?;

    let client = kube_client_from_layer_config(&layer_config).await?;

    let mysql_api: Api<MysqlBranchDatabase> = get_api(args, &client, &layer_config);

    let pg_api: Api<PgBranchDatabase> = get_api(args, &client, &layer_config);

    let list_params = ListParams::default();

    let mysql_branches = mysql_api.list(&list_params).await.map_err(|e| {
        status_progress.failure(Some("failed to list MySQL branches"));
        crate::CliError::ListTargetsFailed(mirrord_kube::error::KubeApiError::KubeError(e))
    })?;

    let pg_branches = pg_api.list(&list_params).await.map_err(|e| {
        status_progress.failure(Some("failed to list PostgreSQL branches"));
        crate::CliError::ListTargetsFailed(mirrord_kube::error::KubeApiError::KubeError(e))
    })?;

    let mut all_branches = Vec::with_capacity(mysql_branches.items.len() + pg_branches.items.len());

    // Process MySQL branches
    for branch in &mysql_branches.items {
        if let Some(name) = &branch.metadata.name
            && (names.is_empty() || names.contains(name))
        {
            all_branches.push(BranchInfo::from(branch));
        }
    }

    // Process PostgreSQL branches
    for branch in &pg_branches.items {
        if let Some(name) = &branch.metadata.name
            && (names.is_empty() || names.contains(name))
        {
            all_branches.push(BranchInfo::from(branch));
        }
    }

    if all_branches.is_empty() {
        progress.success(Some("No active DB branch found"));
        return Ok(());
    }

    status_progress.success(Some("fetched status"));
    progress.success(None);

    let mut table = Table::new();
    table.add_row(row![
        "Name",
        "DB Type",
        "Phase",
        "TTL (sec)",
        "Database",
        "Users",
        "Expires At"
    ]);

    for branch in all_branches {
        table.add_row(row![
            branch.name,
            branch.db_type,
            branch.phase.unwrap_or_else(|| "Unknown".to_string()),
            branch.ttl,
            branch.database.unwrap_or_else(|| "<none>".to_string()),
            branch.users.unwrap_or_else(|| "none".to_string()),
            branch.expire_time.unwrap_or_else(|| "Unknown".to_string())
        ]);
    }

    table.printstd();

    Ok(())
}

async fn destroy_command(args: &DbBranchesArgs, all: bool, names: &Vec<String>) -> CliResult<()> {
    let mut progress = ProgressTracker::from_env("DB Branches Destroy");
    let mut destroy_progress = progress.subtask("deleting branches");

    let mut cfg_context = ConfigContext::default()
        .override_env_opt(LayerConfig::FILE_PATH_ENV, args.config_file.clone())
        .override_env_opt("MIRRORD_TARGET_NAMESPACE", args.namespace.clone());

    let layer_config = LayerConfig::resolve(&mut cfg_context)?;

    let client = kube_client_from_layer_config(&layer_config).await?;

    let mysql_api: Api<MysqlBranchDatabase> = get_api(args, &client, &layer_config);

    let pg_api: Api<PgBranchDatabase> = get_api(args, &client, &layer_config);

    if all {
        // List all branches first to check if any exist
        let mysql_branches = mysql_api.list(&ListParams::default()).await.map_err(|e| {
            crate::CliError::ListTargetsFailed(mirrord_kube::error::KubeApiError::KubeError(e))
        })?;

        let pg_branches = pg_api.list(&ListParams::default()).await.map_err(|e| {
            crate::CliError::ListTargetsFailed(mirrord_kube::error::KubeApiError::KubeError(e))
        })?;

        if mysql_branches.items.is_empty() && pg_branches.items.is_empty() {
            destroy_progress.success(Some("No active DB branch found."));
        }

        // Delete all MySQL branches
        let delete_params = DeleteParams::default();
        for branch in mysql_branches {
            if let Some(name) = branch.metadata.name {
                let mut branch_progress =
                    destroy_progress.subtask(&format!("destroying MySQL branch {name}"));
                match mysql_api.delete(&name, &delete_params).await {
                    Ok(_) => branch_progress.success(Some(&format!("destroyed {name}"))),
                    Err(e) => branch_progress.failure(Some(&format!("failed: {e}"))),
                }
            }
        }

        // Delete all PostgreSQL branches
        for branch in pg_branches {
            if let Some(name) = branch.metadata.name {
                let mut branch_progress =
                    destroy_progress.subtask(&format!("destroying PostgreSQL branch {name}"));
                match pg_api.delete(&name, &delete_params).await {
                    Ok(_) => branch_progress.success(Some(&format!("destroyed {name}"))),
                    Err(e) => branch_progress.failure(Some(&format!("failed: {e}"))),
                }
            }
        }
        destroy_progress.success(None);
    } else {
        // First, list all branches to determine their types
        let mysql_branches = mysql_api.list(&ListParams::default()).await.map_err(|e| {
            crate::CliError::ListTargetsFailed(mirrord_kube::error::KubeApiError::KubeError(e))
        })?;

        let pg_branches = pg_api.list(&ListParams::default()).await.map_err(|e| {
            crate::CliError::ListTargetsFailed(mirrord_kube::error::KubeApiError::KubeError(e))
        })?;

        let mysql_names: HashSet<String> = mysql_branches
            .into_iter()
            .filter_map(|b| b.metadata.name)
            .collect();

        let pg_names: HashSet<String> = pg_branches
            .into_iter()
            .filter_map(|b| b.metadata.name)
            .collect();

        let delete_params = DeleteParams::default();

        for name in names {
            let mut branch_progress = destroy_progress.subtask(&format!("destroying {name}"));

            let mut found = false;

            if mysql_names.contains(name) {
                found = true;
                match mysql_api.delete(name, &delete_params).await {
                    Ok(_) => {
                        branch_progress.success(Some(&format!("destroyed MySQL branch: {name}")))
                    }
                    Err(e) => branch_progress
                        .failure(Some(&format!("failed to destroy MySQL branch {name}: {e}"))),
                }
            }

            if pg_names.contains(name) {
                found = true;
                match pg_api.delete(name, &delete_params).await {
                    Ok(_) => branch_progress
                        .success(Some(&format!("destroyed PostgreSQL branch: {name}"))),
                    Err(e) => branch_progress.failure(Some(&format!(
                        "failed to destroy PostgreSQL branch {name}: {e}"
                    ))),
                }
            }

            if !found {
                branch_progress.failure(Some(&format!("branch not found: {name}")));
            }
        }
        destroy_progress.success(None);
    }

    progress.success(None);

    Ok(())
}
